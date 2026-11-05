//! Desktop host clipboard service. OS reads stay in a kill-on-drop subprocess;
//! bounded handles, ownership and UTF-8 chunking use the same rules as Android.
use crate::{
    clipboard_handles::{Board, Handles, Method, Reading},
    platform_services::{CLIPBOARD_SERVICE, Call, PlatformServices, Response, SelectedInput},
};
use peersus_api::message::Outcome;
use peersus_clipboard_platform::protocol::{Answer, MAX_FRAME, Request};
use peersus_core::plugins::PluginId;
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    process::{Child, ChildStdin, ChildStdout},
    sync::watch,
};

struct Process {
    _child: Child,
    input: ChildStdin,
    output: ChildStdout,
    reader: tokio::task::JoinHandle<()>,
    notices: watch::Sender<Notice>,
}

impl Drop for Process {
    fn drop(&mut self) {
        self.reader.abort();
        self.notices.send_modify(|notice| {
            notice.supported = false;
            notice.generation = notice.generation.wrapping_add(1);
        });
    }
}

#[derive(Clone, Copy, Default)]
struct Notice {
    supported: bool,
    generation: u64,
}

async fn read_notices<R: tokio::io::AsyncRead + Unpin>(
    mut metadata: R,
    notices: watch::Sender<Notice>,
) {
    let mut ready = false;
    while let Ok(byte) = metadata.read_u8().await {
        let supported = match (ready, byte) {
            (false, 1) => {
                ready = true;
                true
            }
            (_, 0) => {
                ready = true;
                false
            }
            (true, 2) => notices.borrow().supported,
            _ => break,
        };
        notices.send_modify(|notice| {
            notice.supported = supported;
            notice.generation = notice.generation.wrapping_add(1);
        });
    }
    notices.send_modify(|notice| {
        notice.supported = false;
        notice.generation = notice.generation.wrapping_add(1);
    });
}

struct DesktopBoard {
    executable: PathBuf,
    process: tokio::sync::Mutex<Option<Process>>,
    notices: watch::Sender<Notice>,
}

impl DesktopBoard {
    async fn request(&self, request: Request) -> Result<Answer, &'static str> {
        tokio::time::timeout(Duration::from_secs(8), self.exchange(request))
            .await
            .unwrap_or(Err("Clipboard helper timed out"))
    }

    async fn exchange(&self, request: Request) -> Result<Answer, &'static str> {
        let mut slot = self.process.lock().await;
        // A cancelled/failed exchange drops and kills this process: never reuse a
        // partial frame or run a queued write after its caller has disappeared.
        let mut process = match slot.take() {
            Some(process) => process,
            None => {
                let mut child = tokio::process::Command::new(&self.executable)
                    .arg("--clipboard-platform-helper")
                    .stdin(std::process::Stdio::piped())
                    .stdout(std::process::Stdio::piped())
                    .stderr(std::process::Stdio::piped())
                    .kill_on_drop(true)
                    .spawn()
                    .map_err(|_| "Cannot start the clipboard helper")?;
                let metadata = child
                    .stderr
                    .take()
                    .ok_or("Cannot watch the clipboard helper")?;
                let notices = self.notices.clone();
                let reader = tokio::spawn(read_notices(metadata, notices));
                Process {
                    reader,
                    notices: self.notices.clone(),
                    input: child
                        .stdin
                        .take()
                        .ok_or("Cannot reach the clipboard helper")?,
                    output: child
                        .stdout
                        .take()
                        .ok_or("Cannot reach the clipboard helper")?,
                    _child: child,
                }
            }
        };
        let bytes = serde_json::to_vec(&request).map_err(|_| "Invalid clipboard request")?;
        if bytes.len() > MAX_FRAME {
            return Err("Oversized clipboard request");
        }
        process
            .input
            .write_u32(bytes.len() as u32)
            .await
            .map_err(|_| "Clipboard helper stopped")?;
        process
            .input
            .write_all(&bytes)
            .await
            .map_err(|_| "Clipboard helper stopped")?;
        process
            .input
            .flush()
            .await
            .map_err(|_| "Clipboard helper stopped")?;
        let size = process
            .output
            .read_u32()
            .await
            .map_err(|_| "Clipboard helper stopped")? as usize;
        if size == 0 || size > MAX_FRAME {
            return Err("Invalid clipboard response");
        }
        let mut bytes = vec![0; size];
        process
            .output
            .read_exact(&mut bytes)
            .await
            .map_err(|_| "Clipboard helper stopped")?;
        let answer: Answer =
            serde_json::from_slice(&bytes).map_err(|_| "Invalid clipboard response")?;
        if answer
            .text
            .as_ref()
            .is_some_and(|text| text.len() > peersus_clipboard_platform::MAX_TEXT)
        {
            return Err("Oversized clipboard response");
        }
        *slot = Some(process);
        Ok(answer)
    }
}

impl Board for DesktopBoard {
    async fn read(&self, observe: bool) -> Reading {
        match self.request(Request::Read { observe }).await {
            Ok(answer) => Reading {
                text: answer.text.filter(|_| answer.ok),
                changed: answer.changed,
            },
            Err(_) => Reading {
                text: None,
                changed: false,
            },
        }
    }
    async fn write(&self, text: String) -> bool {
        self.request(Request::Write { text })
            .await
            .is_ok_and(|answer| answer.ok)
    }
    async fn reset(&self) -> bool {
        self.request(Request::Reset)
            .await
            .is_ok_and(|answer| answer.ok)
    }
}

#[derive(Default)]
struct State {
    handles: Handles,
    next: u64,
    generations: HashMap<PluginId, u64>,
    selected: HashMap<u64, Selection>,
}

struct Selection {
    caller: PluginId,
    text: String,
    expires: Instant,
}

#[derive(Clone)]
pub struct DesktopClipboard {
    board: Arc<DesktopBoard>,
    state: Arc<Mutex<State>>,
}

impl DesktopClipboard {
    pub fn new(executable: PathBuf) -> Arc<Self> {
        Arc::new(Self {
            board: Arc::new(DesktopBoard {
                executable,
                process: tokio::sync::Mutex::new(None),
                notices: watch::channel(Notice::default()).0,
            }),
            state: Arc::new(Mutex::new(State::default())),
        })
    }

    fn state(&self) -> Result<std::sync::MutexGuard<'_, State>, &'static str> {
        self.state
            .lock()
            .map_err(|_| "Clipboard service unavailable")
    }

    pub(crate) async fn copy_result(&self, text: String) -> Result<(), &'static str> {
        if self.board.write(text).await {
            Ok(())
        } else {
            Err("The result link could not be copied")
        }
    }

    async fn execute(&self, call: Call) -> Result<Value, &'static str> {
        if call.service != CLIPBOARD_SERVICE {
            return Err("This platform service is unavailable");
        }
        if call.method == "selected" {
            let context = call
                .args
                .get("context")
                .and_then(Value::as_u64)
                .ok_or("Missing selected input context")?;
            let mut state = self.state()?;
            state.handles.may_capture(&call.caller)?;
            let selection = state
                .selected
                .get(&context)
                .filter(|selection| {
                    selection.caller == call.caller && selection.expires > Instant::now()
                })
                .ok_or("Selected input is unavailable")?;
            let text = selection.text.clone();
            let handle = format!("selected-{context}");
            let result = state.handles.hold_selected(&call.caller, handle, text)?;
            state.selected.remove(&context);
            return Ok(result);
        }
        let method = Method::parse(&call.method).ok_or("Unknown clipboard method")?;
        match method {
            Method::Read | Method::Observe => {
                let generation = {
                    let state = self.state()?;
                    state.handles.may_capture(&call.caller)?;
                    *state.generations.get(&call.caller).unwrap_or(&0)
                };
                let reading = self.board.read(matches!(method, Method::Observe)).await;
                let mut state = self.state()?;
                if *state.generations.get(&call.caller).unwrap_or(&0) != generation {
                    return Err("Clipboard request was revoked");
                }
                state.next = state
                    .next
                    .checked_add(1)
                    .ok_or("Clipboard handle limit reached")?;
                let handle = format!("clip-{}", state.next);
                state.handles.held(&call.caller, handle, reading)
            }
            Method::Chunk => self.state()?.handles.chunk(&call.caller, &call.args),
            Method::Write => {
                let mut state = self.state()?;
                state.next = state
                    .next
                    .checked_add(1)
                    .ok_or("Clipboard handle limit reached")?;
                let handle = format!("put-{}", state.next);
                state.handles.begin(&call.caller, handle, &call.args)
            }
            Method::Append => self.state()?.handles.append(&call.caller, &call.args),
            Method::Commit => {
                let text = self.state()?.handles.finish(&call.caller, &call.args)?;
                crate::clipboard_handles::commit(self.board.as_ref(), text).await
            }
            Method::Reset => Ok(crate::clipboard_handles::reset(self.board.as_ref()).await),
            Method::Release => {
                self.state()?.handles.release(&call.caller);
                Ok(json!({"released":true}))
            }
        }
    }
}

impl PlatformServices for DesktopClipboard {
    fn dispatch(&self, call: Call) -> Response {
        let service = self.clone();
        Box::pin(async move {
            match service.execute(call).await {
                Ok(value) => Outcome::ok(value),
                Err(reason) => Outcome::error("platform", reason),
            }
        })
    }

    fn select(&self, caller: &PluginId, input: SelectedInput) -> Result<u64, &'static str> {
        let SelectedInput::Text(text) = input else {
            return Err("This service accepts only selected text");
        };
        if text.is_empty() || text.len() > peersus_clipboard_platform::MAX_TEXT {
            return Err("Invalid selected text size");
        }
        let mut state = self.state()?;
        state
            .selected
            .retain(|_, selection| selection.expires > Instant::now());
        if state.selected.len() >= 4
            || state
                .selected
                .values()
                .any(|selection| selection.caller == *caller)
        {
            return Err("Release the selected input before selecting another");
        }
        let context = crate::platform_services::next_context()?;
        state.selected.insert(
            context,
            Selection {
                caller: caller.clone(),
                text,
                expires: Instant::now() + crate::clipboard_handles::LIFETIME,
            },
        );
        Ok(context)
    }

    fn discard_selection(&self, caller: &PluginId, context: u64) {
        if let Ok(mut state) = self.state.lock()
            && state
                .selected
                .get(&context)
                .is_some_and(|selection| selection.caller == *caller)
        {
            state.selected.remove(&context);
        }
    }

    fn changes(&self, service: &str) -> Option<watch::Receiver<u64>> {
        if service != CLIPBOARD_SERVICE {
            return None;
        }
        let (sender, receiver) = watch::channel(0_u64);
        let board = self.board.clone();
        let mut notices = board.notices.subscribe();
        tokio::spawn(async move {
            // Ready never reads contents or resets the guest's observation.
            // Cancelling startup also kills a partially started helper.
            tokio::select! {
                _ = sender.closed() => {
                    board.process.lock().await.take();
                    return;
                },
                _ = board.request(Request::Ready) => {},
            }
            let mut ticks = tokio::time::interval(Duration::from_millis(500));
            ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                let supported = notices.borrow().supported;
                tokio::select! {
                    _ = sender.closed() => break,
                    result = notices.changed() => {
                        if result.is_err() { break; }
                        sender.send_modify(|counter| *counter = counter.wrapping_add(1));
                    }
                    _ = ticks.tick(), if !supported => {
                        sender.send_modify(|counter| *counter = counter.wrapping_add(1));
                    }
                }
            }
            board.process.lock().await.take();
        });
        Some(receiver)
    }

    fn revoke(&self, caller: &PluginId) {
        if let Ok(mut state) = self.state.lock() {
            state.handles.release(caller);
            state
                .selected
                .retain(|_, selection| selection.caller != *caller);
            let generation = state.generations.entry(caller.clone()).or_default();
            *generation = generation.wrapping_add(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn notice_change(receiver: &mut watch::Receiver<Notice>) -> Notice {
        tokio::time::timeout(Duration::from_secs(2), receiver.changed())
            .await
            .unwrap()
            .unwrap();
        *receiver.borrow_and_update()
    }

    #[tokio::test]
    async fn metadata_ready_copy_loss_and_end_control_polling_without_contents() {
        let (mut producer, metadata) = tokio::io::duplex(8);
        let (sender, mut receiver) = watch::channel(Notice::default());
        let reader = tokio::spawn(read_notices(metadata, sender));
        producer.write_all(&[1]).await.unwrap();
        let ready = notice_change(&mut receiver).await;
        assert!(ready.supported);
        producer.write_all(&[2]).await.unwrap();
        let copied = notice_change(&mut receiver).await;
        assert!(copied.supported && copied.generation != ready.generation);
        producer.write_all(&[0]).await.unwrap();
        assert!(!notice_change(&mut receiver).await.supported);
        producer.write_all(&[2]).await.unwrap();
        assert!(
            !notice_change(&mut receiver).await.supported,
            "an overflow cannot silently re-enable event-only observation"
        );
        drop(producer);
        reader.await.unwrap();
        assert!(!receiver.borrow().supported);
    }

    #[tokio::test]
    async fn metadata_unsupported_eof_and_cancellation_release_the_pipe() {
        let (mut producer, metadata) = tokio::io::duplex(8);
        let (sender, mut receiver) = watch::channel(Notice::default());
        let reader = tokio::spawn(read_notices(metadata, sender));
        producer.write_all(&[0]).await.unwrap();
        assert!(!notice_change(&mut receiver).await.supported);
        reader.abort();
        assert!(reader.await.unwrap_err().is_cancelled());
        assert!(producer.write_all(&[2]).await.is_err());

        let (producer, metadata) = tokio::io::duplex(8);
        let (sender, receiver) = watch::channel(Notice::default());
        drop(producer);
        read_notices(metadata, sender).await;
        assert!(!receiver.borrow().supported);
    }

    #[tokio::test]
    async fn notice_subscription_closes_without_starting_an_os_reader() {
        let service = DesktopClipboard::new(PathBuf::from("/no-clipboard-helper"));
        let mut changes = service.changes(CLIPBOARD_SERVICE).unwrap();
        tokio::time::timeout(Duration::from_secs(2), changes.changed())
            .await
            .unwrap()
            .unwrap();
        drop(changes);
        tokio::task::yield_now().await;
        assert!(service.board.process.lock().await.is_none());
    }

    fn caller(name: &str) -> PluginId {
        PluginId::new(name).unwrap()
    }
    fn selected(caller: &PluginId, context: u64) -> Call {
        Call {
            caller: caller.clone(),
            service: CLIPBOARD_SERVICE.into(),
            method: "selected".into(),
            args: json!({"context":context}),
        }
    }

    #[tokio::test]
    async fn selected_text_is_caller_scoped_single_use_and_never_reads_the_system() {
        // A nonexistent helper makes accidental system access impossible.
        let service = DesktopClipboard::new(PathBuf::from("/no-clipboard-helper"));
        let (one, two) = (caller("one"), caller("two"));
        let context = service
            .select(&one, SelectedInput::Text("selected é🙂".into()))
            .unwrap();
        assert!(service.execute(selected(&two, context)).await.is_err());
        let capture = service.execute(selected(&one, context)).await.unwrap();
        assert!(service.execute(selected(&one, context)).await.is_err());
        let piece = service
            .execute(Call {
                caller: one,
                service: CLIPBOARD_SERVICE.into(),
                method: "chunk".into(),
                args: json!({"clip":capture["clip"],"offset":0}),
            })
            .await
            .unwrap();
        assert_eq!(piece["text"], "selected é🙂");
    }

    #[tokio::test]
    async fn revocation_and_expiry_make_selected_inputs_unusable() {
        let service = DesktopClipboard::new(PathBuf::from("/no-clipboard-helper"));
        let one = caller("one");
        let context = service
            .select(&one, SelectedInput::Text("first".into()))
            .unwrap();
        service.revoke(&one);
        assert!(service.execute(selected(&one, context)).await.is_err());
        let context = service
            .select(&one, SelectedInput::Text("second".into()))
            .unwrap();
        service
            .state()
            .unwrap()
            .selected
            .get_mut(&context)
            .unwrap()
            .expires = Instant::now();
        assert!(service.execute(selected(&one, context)).await.is_err());
        assert!(
            service.board.process.lock().await.is_none(),
            "selected text never opens the OS clipboard"
        );
    }
}
