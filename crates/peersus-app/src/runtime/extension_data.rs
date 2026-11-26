use super::*;
use crate::extension_data::{self as data, random_scope};
use peersus_api::{
    audio, extension_data as api,
    message::{Operation, Outcome},
    pictures,
};
use peersus_core::profile::Kept;
use serde_json::{Value, json};

impl Runtime {
    pub(super) fn extension_data_call(
        &mut self,
        ticket: u64,
        caller: &str,
        service: &str,
        method: &str,
        args: &Value,
    ) -> bool {
        let file = service == api::FILES && matches!(method, api::STAGE_SELECTED | api::SAVE);
        let picture = service == pictures::SERVICE
            && matches!(method, pictures::KEEP | pictures::FORGET | pictures::DECODE);
        let sound = service == audio::SERVICE
            && matches!(
                method,
                audio::PLAY
                    | audio::QUEUE
                    | audio::PAUSE
                    | audio::RESUME
                    | audio::STOP
                    | audio::STATE
                    | audio::DECODE
                    | audio::RECORD
                    | audio::FINISH
                    | audio::CANCEL
                    | audio::LEVELS
            );
        if !(file
            || picture
            || sound
            || service == api::STORAGE
            || service == api::CLOCK
            || service == api::PEERS && matches!(method, api::NEW_SCOPE | api::REFERENCES))
        {
            return false;
        }
        let result = self.begin_extension_data(ticket, caller, service, method, args);
        if let Some(result) = result {
            let _ = self.host.submit(Operation::Reply { ticket, result });
        }
        true
    }

    fn begin_extension_data(
        &mut self,
        ticket: u64,
        caller: &str,
        service: &str,
        method: &str,
        args: &Value,
    ) -> Option<Outcome> {
        let invalid = || Outcome::error("invalid", "Invalid extension data request");
        let unavailable =
            || Outcome::error("unavailable", "The extension data facility is unavailable");
        let busy = |reason| Outcome::error("busy", reason);
        let Ok(caller) = PluginId::new(caller) else {
            return Some(invalid());
        };
        self.host.pending_caller_generation(ticket)?;
        if (service, method) == (api::CLOCK, api::NOW) {
            return Some(
                if serde_json::from_value::<api::Empty>(args.clone()).is_ok() {
                    // Quantize elapsed-only time. Event timestamps from other interfaces are unrelated.
                    Outcome::ok(
                        json!({"monotonic_ms": super::plugin_calls::owner_clock() / 10 * 10}),
                    )
                } else {
                    invalid()
                },
            );
        }
        if (service, method) == (api::CLOCK, api::WALL) {
            return Some(
                if serde_json::from_value::<api::Empty>(args.clone()).is_ok() {
                    match wall_time(jiff::ZonedDateTime::now()) {
                        Some(now) => Outcome::ok(json!(now)),
                        None => unavailable(),
                    }
                } else {
                    invalid()
                },
            );
        }
        if (service, method) == (api::PEERS, api::NEW_SCOPE) {
            return Some(
                if serde_json::from_value::<api::Empty>(args.clone()).is_ok() {
                    Outcome::ok(json!({"scope":random_scope()}))
                } else {
                    invalid()
                },
            );
        }
        if service == pictures::SERVICE && method != pictures::KEEP {
            if self.plugins.find(&caller).is_none() {
                return Some(invalid());
            }
            if method == pictures::DECODE {
                return Some(Outcome::error(
                    "unsupported",
                    "Pictures are decoded in the extension's own process",
                ));
            }
            let Ok(forget) = pictures::Forget::parse(args) else {
                return Some(invalid());
            };
            self.kept_pictures().forget(&caller, &forget);
            self.publish_pictures_if_changed();
            return Some(Outcome::ok(json!({})));
        }
        if service == audio::SERVICE && method == audio::RECORD {
            let Ok(audio::Record { max_seconds }) = audio::Record::parse(args) else {
                return Some(invalid());
            };
            if self.plugins.find(&caller).is_none() {
                return Some(invalid());
            }
            let microphone = self.microphone.clone();
            let platform = self.platform_services.clone();
            let recorder = caller.clone();
            let permission = Permission::new(HOST, audio::RECORD_CAPABILITY);
            return self
                .platform_calls
                .start(ticket, caller, permission, move || async move {
                    // The system's own question first, where it has one; the person's grant was checked by the hub.
                    if let Some(platform) = platform
                        && let Err(reason) = platform.microphone_allowed().await
                    {
                        return Outcome::error("refused", reason);
                    }
                    match microphone.start(&recorder, max_seconds) {
                        Ok(()) => Outcome::ok(json!({})),
                        Err(reason) => Outcome::error("busy", reason),
                    }
                })
                .err()
                .map(busy);
        }
        if service == audio::SERVICE
            && !matches!(method, audio::PLAY | audio::QUEUE | audio::FINISH)
        {
            if self.plugins.find(&caller).is_none()
                || serde_json::from_value::<api::Empty>(args.clone()).is_err()
            {
                return Some(invalid());
            }
            if method == audio::CANCEL {
                self.microphone.clear(&caller);
                return Some(Outcome::ok(json!({})));
            }
            if method == audio::LEVELS {
                return Some(match self.microphone.levels(&caller) {
                    Some((elapsed_ms, levels, bands)) => serde_json::to_value(audio::Levels {
                        elapsed_ms,
                        levels,
                        bands,
                    })
                    .map_or_else(|_| unavailable(), Outcome::ok),
                    None => Outcome::error("unavailable", "No recording is running"),
                });
            }
            return Some(match method {
                audio::DECODE => Outcome::error(
                    "unsupported",
                    "Sounds are decoded in the extension's own process",
                ),
                audio::STATE => match serde_json::to_value(self.sound.state(&caller)) {
                    Ok(state) => Outcome::ok(state),
                    Err(_) => unavailable(),
                },
                _ => {
                    match method {
                        audio::PAUSE => self.sound.pause(&caller, true),
                        audio::RESUME => self.sound.pause(&caller, false),
                        _ => self.sound.stop(&caller),
                    }
                    Outcome::ok(json!({}))
                }
            });
        }
        let Some(storage) = self.extension_data.clone() else {
            return Some(unavailable());
        };
        let Some(program) = self
            .plugins
            .find(&caller)
            .and_then(|i| self.plugins.get(i))
            .and_then(|spec| spec.program.clone())
        else {
            return Some(unavailable());
        };
        let access = match storage.access(caller.clone()) {
            Ok(access) => access.for_program(program),
            Err(error) => return Some(error.outcome()),
        };
        if service == pictures::SERVICE {
            let Ok(pictures::Keep {
                id,
                width,
                height,
                source,
            }) = pictures::Keep::parse(args)
            else {
                return Some(invalid());
            };
            let bytes = usize::from(width) * usize::from(height) * 4;
            let generation = self.kept_pictures().generation(&caller);
            let kept = self.pictures.clone();
            let keeper = caller.clone();
            return self
                .platform_calls
                .start(ticket, caller, None, move || async move {
                    let rgba = match storage.read_whole(access, source, bytes).await {
                        Ok(rgba) => rgba,
                        Err(error) => return error.outcome(),
                    };
                    let Ok(picture) = Kept::new(u32::from(width), u32::from(height), rgba) else {
                        return Outcome::error("invalid", "Invalid extension data request");
                    };
                    let kept = kept
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .keep(&keeper, generation, id, picture);
                    match kept {
                        Ok(()) => Outcome::ok(json!({})),
                        Err(reason) => Outcome::error("refused", reason),
                    }
                })
                .err()
                .map(busy);
        }
        if (service, method) == (audio::SERVICE, audio::FINISH) {
            if serde_json::from_value::<api::Empty>(args.clone()).is_err() {
                return Some(invalid());
            }
            let (samples, levels) = match self.microphone.finish(&caller) {
                Ok(recorded) => recorded,
                Err(reason) => return Some(Outcome::error("unavailable", reason)),
            };
            let duration_ms = samples.len() as u64 * 1000 / u64::from(audio::RECORD_RATE);
            return self
                .platform_calls
                .start(ticket, caller, None, move || async move {
                    match storage.import_bytes(access, audio::wav(&samples)).await {
                        Ok(api::Sealed {
                            stage,
                            bytes,
                            sha256,
                        }) => serde_json::to_value(audio::Recorded {
                            stage,
                            bytes,
                            sha256,
                            duration_ms,
                            levels,
                        })
                        .map_or_else(
                            |_| Outcome::error("unavailable", "The recording could not be kept"),
                            Outcome::ok,
                        ),
                        Err(error) => error.outcome(),
                    }
                })
                .err()
                .map(busy);
        }
        if service == audio::SERVICE {
            let Ok(audio::Chunk {
                sound,
                bytes,
                source,
            }) = audio::Chunk::parse(args)
            else {
                return Some(invalid());
            };
            let output = self.sound.clone();
            let generation = output.generation(&caller);
            let player = caller.clone();
            let starts = method == audio::PLAY;
            return self
                .platform_calls
                .start(ticket, caller, None, move || async move {
                    let samples = match storage.read_whole(access, source, bytes as usize).await {
                        Ok(samples) => samples,
                        Err(error) => return error.outcome(),
                    };
                    let handed = if starts {
                        output.play(&player, generation, sound, &samples)
                    } else {
                        output.queue(&player, generation, &sound, &samples)
                    };
                    match handed {
                        Ok(()) => Outcome::ok(json!({})),
                        Err(reason) => Outcome::error("refused", reason),
                    }
                })
                .err()
                .map(busy);
        }
        if service == api::STORAGE {
            let mut fence = None;
            if let Some(lease) = args.get("receive_ticket").filter(|v| !v.is_null()) {
                if method != "commit" {
                    return Some(invalid());
                }
                fence = lease.as_u64().and_then(|id| {
                    self.receiving.values().find_map(|entry| {
                        (entry.owner.plugin == caller)
                            .then_some(entry.data_commit.as_ref())
                            .flatten()
                            .filter(|(ticket, _)| *ticket == id)
                            .map(|(_, fence)| fence.clone())
                    })
                });
                if fence.is_none() {
                    return Some(Outcome::error(
                        "cancelled",
                        "The receive call is no longer active",
                    ));
                }
            }
            let (method, args) = (method.to_owned(), args.clone());
            return self
                .platform_calls
                .start(ticket, caller, None, move || {
                    storage.dispatch(access, method, args, fence)
                })
                .err()
                .map(busy);
        }
        if (service, method) == (api::PEERS, api::REFERENCES) {
            let Ok(request) = serde_json::from_value::<api::References>(args.clone()) else {
                return Some(invalid());
            };
            if !request.valid() {
                return Some(invalid());
            }
            let peers = self.network.peers();
            let visible: BTreeMap<_, _> = peers
                .iter()
                .filter(|p| super::permissions::seen_by(p, &caller).is_some())
                .map(|peer| (self.aliases.alias(&caller, &peer.id), peer))
                .collect();
            let mut requested = Vec::with_capacity(request.devices.len());
            for alias in request.devices {
                let Some(peer) = visible.get(&alias) else {
                    return Some(Outcome::error(
                        "not-found",
                        "The device is no longer visible",
                    ));
                };
                requested.push((alias, peer.id.clone(), peer.paired_at));
            }
            let local = request.include_self.then(|| {
                (
                    self.network.local_id(),
                    self.settings.current().device_name.clone(),
                )
            });
            return self
                .platform_calls
                .start(ticket, caller, None, move || async move {
                    match storage
                        .references(access, requested, local, request.scope)
                        .await
                    {
                        Ok(value) => {
                            serde_json::to_value(value).map_or_else(|_| unavailable(), Outcome::ok)
                        }
                        Err(error) => error.outcome(),
                    }
                })
                .err()
                .map(busy);
        }
        let Some(platform) = self.platform_services.clone() else {
            return Some(unavailable());
        };
        let capability = if method == api::SAVE {
            api::EXPORT
        } else {
            crate::platform_services::FILES_CAPABILITY
        };
        let Some(permission) = Permission::new(HOST, capability) else {
            return Some(invalid());
        };
        if !self
            .standalone
            .get(&caller)
            .is_some_and(|held| held.allows(&permission))
        {
            self.warn_missing_permission(&caller, permission.clone(), None);
            return Some(Outcome::MissingPermission { permission });
        }
        if method == api::STAGE_SELECTED {
            let Ok(request) = serde_json::from_value::<api::Selected>(args.clone()) else {
                return Some(invalid());
            };
            let Some(selected) = platform.file_snapshot(&caller, &request.file) else {
                return Some(invalid());
            };
            return self
                .platform_calls
                .start(ticket, caller, Some(permission), move || async move {
                    match storage.import_selected(access, selected).await {
                        Ok(value) => {
                            serde_json::to_value(value).map_or_else(|_| unavailable(), Outcome::ok)
                        }
                        Err(error) => error.outcome(),
                    }
                })
                .err()
                .map(busy);
        }
        let Ok(request) = serde_json::from_value::<api::Save>(args.clone()) else {
            return Some(invalid());
        };
        if !request.valid() {
            return Some(invalid());
        }
        self.platform_calls
            .start(
                ticket,
                caller.clone(),
                Some(permission),
                move || async move {
                    let guard = data::Fence::new();
                    struct Cancel(data::Fence);
                    impl Drop for Cancel {
                        fn drop(&mut self) {
                            self.0.revoke();
                        }
                    }
                    let _cancel = Cancel(guard.clone());
                    let (source, bytes) = match storage
                        .export_source(access, request.key, request.revision)
                        .await
                    {
                        Ok(value) => value,
                        Err(error) => return error.outcome(),
                    };
                    let destination = match platform
                        .save_extension_file(caller, request.suggested_name)
                        .await
                    {
                        Ok(file) => file,
                        Err(_) => return Outcome::error("cancelled", "No file was saved"),
                    };
                    match storage.export_copy(source, destination, bytes, guard).await {
                        Ok(()) => Outcome::ok(json!({"saved":true})),
                        Err(error) => error.outcome(),
                    }
                },
            )
            .err()
            .map(busy)
    }
}

/// The minute `now` is and its time zone's distance from UTC, to the minute and no finer, so the answer is as coarse as
/// what it is for (showing when something happened); `None` for a time the contract cannot carry.
fn wall_time(now: jiff::ZonedDateTime) -> Option<api::WallTime> {
    let answer = api::WallTime {
        unix_minute: now.timestamp().as_second().div_euclid(60),
        utc_offset_minutes: now.offset().seconds().div_euclid(60),
    };
    answer.valid().then_some(answer)
}

#[cfg(test)]
mod wall_tests {
    use super::*;

    #[test]
    fn the_wall_time_is_the_minute_and_the_zones_offset_and_nothing_finer() {
        // 2026-11-25 07:33:59.999 in Bucharest (UTC+2): the minute 05:33 UTC, offset +120.
        let now: jiff::ZonedDateTime = "2026-11-25T07:33:59.999+02:00[Europe/Bucharest]"
            .parse()
            .unwrap();
        let answer = wall_time(now).unwrap();
        assert_eq!(answer.utc_offset_minutes, 120);
        assert_eq!(answer.unix_minute, now_minute("2026-11-25T05:33:00Z"));
        assert_eq!(answer.local_day_and_minute().1, 7 * 60 + 33);
        // A zone half an hour off and west of UTC keeps its sign and its half hour.
        let now: jiff::ZonedDateTime = "2026-07-01T10:15:30-02:30[America/St_Johns]"
            .parse()
            .unwrap();
        assert_eq!(wall_time(now).unwrap().utc_offset_minutes, -150);
        // Before 1970 the contract carries nothing.
        let now: jiff::ZonedDateTime = "1969-12-31T23:59:00+00:00[UTC]".parse().unwrap();
        assert_eq!(wall_time(now), None);
    }

    fn now_minute(utc: &str) -> i64 {
        utc.parse::<jiff::Timestamp>().unwrap().as_second() / 60
    }
}
