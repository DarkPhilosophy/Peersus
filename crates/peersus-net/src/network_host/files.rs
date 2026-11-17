//! Files and folders: a code to hand files over, the selection a code carries, sending to a paired device, folders walked
//! file by file, accepting and cancelling.
use super::*;

impl NetworkHost {
    /// A code to hand over files, and the files it is for. The sources are parked with the code: when a device that was
    /// shown it is admitted, THIS host offers them on that connection by itself, so nothing waits on a window. A new files
    /// code replaces the previous one, with what was parked for it. Never a pairing: it has its own gate.
    pub fn invite_for_files(
        &mut self,
        sources: Vec<Source>,
        now: u64,
    ) -> Result<Invitation, crate::invitation::Error> {
        let (invitation, generation) =
            self.invitations
                .create_for_file_selection(&self.endpoint, now, sources.len())?;
        // Kept with the generation of THIS code: an admission that was made with an earlier one, and has not finished yet,
        // finds a plan that is not its own and takes nothing.
        self.parked = Some((generation, sources));
        self.selection = None;
        Ok(invitation)
    }

    /// Show one code before opening files. Batches cannot renew its count or lifetime.
    pub fn begin_files_selection(
        &mut self,
        count: usize,
        now: u64,
    ) -> Result<Invitation, crate::invitation::Error> {
        if count == 0 {
            return Err(crate::invitation::Error::Invalid);
        }
        let (invitation, generation) =
            self.invitations
                .create_for_file_selection(&self.endpoint, now, count)?;
        self.parked = None;
        self.selection = Some(FileSelection {
            generation,
            remaining: count,
            until: tokio::time::Instant::now()
                + Duration::from_secs(invitation.expires_at.saturating_sub(now)),
            admitted: None,
            pending: VecDeque::new(),
            finished: false,
        });
        Ok(invitation)
    }

    /// False means backpressure, with no source consumed. An invalid target is an error.
    pub fn can_queue_selection(
        &self,
        generation: u64,
        count: usize,
        _now: u64,
    ) -> io::Result<bool> {
        let selection = self
            .selection
            .as_ref()
            .filter(|s| {
                s.generation == generation && !s.finished && tokio::time::Instant::now() < s.until
            })
            .ok_or_else(|| io::Error::other("Files selection is no longer active"))?;
        if count == 0 || count > selection.remaining {
            return Err(io::Error::other("Files selection allowance exceeded"));
        }
        if let Some((connection, _)) = &selection.admitted
            && connection.close_reason().is_some()
        {
            return Err(io::Error::other("Files selection connection closed"));
        }
        let reserved = selection.pending.len().saturating_add(count);
        Ok(reserved <= crate::transfer_tasks::QUEUED_LIMIT && self.transfers.can_queue(reserved))
    }

    pub fn queue_selection(
        &mut self,
        generation: u64,
        sources: Vec<Source>,
        now: u64,
    ) -> io::Result<()> {
        if !self.can_queue_selection(generation, sources.len(), now)? {
            return Err(io::Error::other("The transfer queue is full"));
        }
        let selection = self.selection.as_mut().expect("validated selection");
        selection.remaining -= sources.len();
        selection.pending.extend(sources);
        Ok(())
    }

    /// Finishing cannot revoke files already queued or reopen a producer.
    pub fn finish_files_selection(&mut self, generation: u64, refused: usize) -> io::Result<()> {
        let selection = self
            .selection
            .as_mut()
            .filter(|s| s.generation == generation && !s.finished)
            .ok_or_else(|| io::Error::other("Files selection is no longer active"))?;
        if selection.remaining != refused {
            return Err(io::Error::other(
                "Files selection is not fully accounted for",
            ));
        }
        selection.remaining = 0;
        selection.finished = true;
        Ok(())
    }

    /// Take back the files code of `generation` with the files that wait behind it, while no device has scanned it (see
    /// [`Invitations::withdraw_files`]). The files are let go of here: whoever asked for this still has what it chose and offers it again where
    /// the person now says. False, with nothing changed, when the code was already scanned, ran out or is not the one on show.
    pub fn withdraw_files_code(&mut self, generation: u64) -> bool {
        if !self.invitations.withdraw_files(generation) {
            return false;
        }
        if self
            .parked
            .as_ref()
            .is_some_and(|(of, _)| *of == generation)
        {
            self.parked = None;
        }
        if self
            .selection
            .as_ref()
            .is_some_and(|selection| selection.generation == generation)
        {
            self.selection = None;
        }
        true
    }

    pub(super) fn feed_file_selection(&mut self, now: u64) -> io::Result<bool> {
        let Some(selection) = self.selection.as_mut() else {
            return Ok(false);
        };
        if tokio::time::Instant::now() >= selection.until
            || selection
                .admitted
                .as_ref()
                .is_some_and(|(connection, _)| connection.close_reason().is_some())
            || (selection.finished && selection.pending.is_empty())
        {
            self.selection = None;
            return Ok(true);
        }
        let Some((connection, name)) = &selection.admitted else {
            return Ok(false);
        };
        let mut changed = false;
        for _ in 0..16 {
            if !self.transfers.can_queue(1) {
                break;
            }
            let Some(source) = selection.pending.pop_front() else {
                break;
            };
            self.transfers.send(
                connection.clone(),
                Some(name.clone()),
                Some(selection.generation),
                source,
                &mut self.history,
                now,
            )?;
            changed = true;
        }
        Ok(changed)
    }

    /// Test a producer batch before opening or taking its sources. Unknown and disconnected
    /// peers are errors, not backpressure: a producer must not wait forever for a stale target.
    pub fn can_queue_files(&self, peer: &PeerId, count: usize) -> io::Result<bool> {
        let session = self
            .sessions
            .get(peer)
            .ok_or_else(|| io::Error::other("Device is offline"))?;
        if session.connection.close_reason().is_some() {
            return Err(io::Error::other("Device is offline"));
        }
        let reserved = self
            .selection
            .as_ref()
            .map_or(0, |selection| selection.pending.len());
        Ok(self.transfers.can_queue(reserved.saturating_add(count)))
    }

    pub fn send(&mut self, peer: &PeerId, source: Source, now: u64) -> io::Result<String> {
        let session = self
            .sessions
            .get(peer)
            .ok_or_else(|| io::Error::other("Device is offline"))?;
        self.transfers.send(
            session.connection.clone(),
            None,
            None,
            source,
            &mut self.history,
            now,
        )
    }

    /// Begin sending the folder `root` to the paired device `peer`: every file in it, with the folders it sits in. Nothing is opened and
    /// nothing is offered here: the folder is walked as the files go (see [`crate::folder_sender`]) and [`Self::feed_folders`] offers one
    /// whenever there is room, so a folder of any size is never held as a list and never as open files.
    ///
    /// Refused for a device that is offline or not paired, and for a `root` that is not a plain folder. Returns the id of this
    /// folder's sending, to ask how it stands and to stop it.
    pub fn send_folder(&mut self, peer: &PeerId, root: &Path) -> io::Result<u64> {
        if !self.sessions.contains_key(peer) {
            return Err(io::Error::other("Device is offline"));
        }
        // Directory walks keep capabilities open. Bound active walks separately from file-transfer slots.
        if self
            .folders
            .iter()
            .filter(|run| !run.sender.is_done())
            .count()
            >= 16
        {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "Folder queue is full",
            ));
        }
        let sender = FolderSender::new(root)?;
        // Bound recent completed results; callers may also dismiss one with stop_folder.
        let mut completed = self
            .folders
            .iter()
            .filter(|run| run.sender.is_done())
            .count();
        self.folders.retain(|run| {
            if run.sender.is_done() && completed >= 16 {
                completed -= 1;
                false
            } else {
                true
            }
        });
        self.folder_serial += 1;
        self.folders.push(FolderRun {
            id: self.folder_serial,
            peer: peer.clone(),
            sender,
        });
        Ok(self.folder_serial)
    }

    /// Offer ONE more file of the folders being sent, if there is room and a folder has one. Returns whether a file was offered, so a
    /// caller that is not told "no more" calls it again. One file per call keeps each step short, which the loop that pumps events
    /// needs: a folder of a million files is a million short steps, not one long one.
    ///
    /// A folder whose device is gone is let go of. Finished walks remain as bounded recent results in
    /// [`Self::folder_state`]; starting another folder evicts the oldest completed results.
    pub async fn feed_folders(&mut self, now: u64) -> bool {
        self.folders
            .retain(|run| self.sessions.contains_key(&run.peer));
        if !self.transfers.has_room() {
            return false;
        }
        let Some(index) = self.folders.iter().position(|run| !run.sender.is_done()) else {
            return false;
        };
        let peer = self.folders[index].peer.clone();
        let Some(source) = self.folders[index].sender.next().await else {
            // The walk is complete; preserve its counters among the bounded recent results.
            return true;
        };
        // A send that is refused (no room after all, the device just went) is not lost silently: the file was opened and is dropped,
        // and it is counted as one that could not be sent.
        if self.send(&peer, source, now).is_err() {
            self.folders[index].sender.count_failed();
        }
        true
    }

    /// How the folders being sent stand: for each, its id, what has been offered and what could not be, and what the walk left out.
    /// At most thirty-two retained walks, no more than sixteen of them still active.
    pub fn folder_state(
        &self,
    ) -> Vec<(
        u64,
        PeerId,
        crate::folder_sender::Sent,
        crate::walk::Passed,
        bool,
    )> {
        self.folders
            .iter()
            .map(|run| {
                (
                    run.id,
                    run.peer.clone(),
                    run.sender.sent(),
                    run.sender.passed(),
                    run.sender.is_done(),
                )
            })
            .collect()
    }

    /// Stop sending a folder: no more of its files are offered. What was already offered stays offered, and is the person's to cancel.
    pub fn stop_folder(&mut self, id: u64) -> bool {
        let before = self.folders.len();
        self.folders.retain(|run| run.id != id);
        self.folders.len() != before
    }
    pub fn accept(&mut self, id: &str, directory: PathBuf) -> io::Result<()> {
        self.transfers.accept(id, directory, &mut self.history)
    }
    pub fn cancel(&mut self, id: &str) -> io::Result<()> {
        self.transfers.cancel(id, &mut self.history)?;
        // A cancelled exchange is over: it stays in the list until the person clears it.
        Ok(())
    }
}
