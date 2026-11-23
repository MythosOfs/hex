//! Labs › Autocorrect (Linux Wayland): fixes clear typos in what you type on
//! a physical keyboard as you finish each word, in German and English at
//! once, and learns your own words as the phone keyboards do.
//!
//! Hex becomes the seat's input method (`zwp_input_method_v2`) while the lab
//! is on, without grabbing the keyboard: keys still go straight to the app,
//! and Hex reads only the text before the cursor that the focused field
//! reports (text-input-v3 surrounding text). Password, PIN, terminal, URL,
//! e-mail, and number fields are skipped. A fix replaces the word through
//! the input method's delete-surrounding and commit; Backspace right after
//! it puts the typed word back and learns it (`autocorrect/field.rs`).
//! While Hex holds the input method, another one (Fcitx5, IBus, or Hex's own
//! Live typing) gets the seat unavailable, and the other way round.

mod correct;
mod field;
mod language_model;
mod learned;
mod model;

use std::os::fd::AsRawFd;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use color_eyre::eyre::{Result, WrapErr};
use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::{wl_registry, wl_seat};
use wayland_client::{Connection, Dispatch, EventQueue, QueueHandle, WEnum};
use wayland_protocols::wp::text_input::zv3::client::zwp_text_input_v3::{
    ContentHint, ContentPurpose,
};

use self::correct::Corrector;
use self::field::Field;
use self::language_model::LanguageModel;
use self::learned::Learned;
use crate::linux::live_typing::protocol::zwp_input_method_manager_v2::ZwpInputMethodManagerV2;
use crate::linux::live_typing::protocol::zwp_input_method_v2::{
    Event as InputMethodEvent, ZwpInputMethodV2,
};

/// How often the thread looks at the lab's switch and Hex's shutdown.
const TICK: Duration = Duration::from_millis(500);
/// How long after the seat was unavailable Hex asks for it again.
const RETRY: Duration = Duration::from_secs(30);
/// Learned words are written at most this often.
const SAVE_EVERY: Duration = Duration::from_secs(5);

/// Starts the autocorrect thread on Wayland; it idles while the lab is off.
pub(crate) fn start(shutdown: &'static AtomicBool) {
    if !crate::linux::session::LinuxSession::detect().is_wayland() {
        return;
    }
    if let Err(error) = thread::Builder::new()
        .name("hex-autocorrect".into())
        .spawn(move || run(shutdown))
    {
        tracing::info!(%error, "autocorrect is unavailable");
    }
}

fn run(shutdown: &'static AtomicBool) {
    let path = crate::app_paths::support_dir()
        .map(|dir| dir.join("autocorrect.json"))
        .ok();
    let mut owner: Option<Owner> = None;
    let mut retry_at = Instant::now();
    let mut saved_at = Instant::now();
    let language = Arc::new(Mutex::new(None));
    // Set while the model downloads or loads; cleared to stop a download.
    let mut loading: Option<Arc<AtomicBool>> = None;
    while !shutdown.load(Ordering::Acquire) {
        let on = crate::labs::autocorrect();
        let wants_model = on && crate::labs::autocorrect_model();
        if wants_model && loading.is_none() {
            loading = Some(load_language_model(&language));
        }
        if !wants_model && let Some(canceled) = loading.take() {
            canceled.store(true, Ordering::Release);
            *language.lock().unwrap() = None;
        }
        if on && owner.is_none() && Instant::now() >= retry_at {
            match Owner::connect(path.clone(), language.clone()) {
                Ok(connected) => owner = Some(connected),
                Err(error) => {
                    tracing::info!(%error, "autocorrect could not become the input method");
                    retry_at = Instant::now() + RETRY;
                }
            }
        }
        if !on && let Some(mut ended) = owner.take() {
            ended.end();
        }
        let Some(current) = &mut owner else {
            thread::sleep(TICK);
            continue;
        };
        if let Err(error) = current.pump(TICK) {
            tracing::info!(%error, "autocorrect lost its compositor connection");
            current.save();
            owner = None;
            retry_at = Instant::now() + RETRY;
            continue;
        }
        if current.state.unavailable {
            tracing::info!("another input method holds the seat; autocorrect waits");
            current.end();
            owner = None;
            retry_at = Instant::now() + RETRY;
            continue;
        }
        if saved_at.elapsed() >= SAVE_EVERY {
            current.save();
            saved_at = Instant::now();
        }
    }
    if let Some(mut ended) = owner {
        ended.end();
    }
    if let Some(canceled) = loading {
        canceled.store(true, Ordering::Release);
    }
    *language.lock().unwrap() = None;
}

/// Downloads and loads the language model on its own thread into
/// `language`; the flag it returns stops it. A failure is logged and not
/// retried until the lab is turned off and on.
fn load_language_model(language: &Arc<Mutex<Option<LanguageModel>>>) -> Arc<AtomicBool> {
    let canceled = Arc::new(AtomicBool::new(false));
    let (language, flag) = (language.clone(), canceled.clone());
    let spawned = thread::Builder::new()
        .name("hex-autocorrect-model".into())
        .spawn(move || {
            let loaded = language_model::fetch(&flag).and_then(|path| LanguageModel::load(&path));
            match loaded {
                Ok(model) if !flag.load(Ordering::Acquire) => {
                    tracing::info!("autocorrect's language model is loaded");
                    *language.lock().unwrap() = Some(model);
                }
                Ok(_) => {}
                Err(error) => tracing::info!(%error, "autocorrect's language model is unavailable"),
            }
        });
    if let Err(error) = spawned {
        tracing::info!(%error, "autocorrect's language model is unavailable");
    }
    canceled
}

struct Owner {
    connection: Connection,
    queue: EventQueue<State>,
    manager: ZwpInputMethodManagerV2,
    method: ZwpInputMethodV2,
    state: State,
    path: Option<PathBuf>,
}

impl Owner {
    fn connect(path: Option<PathBuf>, language: Arc<Mutex<Option<LanguageModel>>>) -> Result<Self> {
        let connection =
            Connection::connect_to_env().wrap_err("there is no Wayland display to reach")?;
        let (globals, mut queue) = registry_queue_init::<State>(&connection)
            .wrap_err("the Wayland registry could not be read")?;
        let handle = queue.handle();
        let seat: wl_seat::WlSeat = globals
            .bind(&handle, 1..=1, ())
            .wrap_err("this Wayland seat is unavailable")?;
        let manager: ZwpInputMethodManagerV2 = globals
            .bind(&handle, 1..=1, ())
            .wrap_err("this compositor offers no zwp_input_method_manager_v2")?;
        let method = manager.get_input_method(&seat, &handle, ());
        let learned = path.as_deref().map(Learned::load).unwrap_or_default();
        let mut state = State::new(Corrector::built_in(), learned, language);
        queue
            .roundtrip(&mut state)
            .wrap_err("the compositor did not answer")?;
        tracing::info!("autocorrect is the input method");
        Ok(Self {
            connection,
            queue,
            manager,
            method,
            state,
            path,
        })
    }

    /// Handles the compositor's events for up to `timeout`, then sends the
    /// fixes they asked for.
    fn pump(&mut self, timeout: Duration) -> Result<()> {
        self.turn(timeout)?;
        // A fix is sent only while no newer text has arrived: once you typed
        // on, deleting the word's bytes before the cursor would hit your new
        // letters.
        while let Some((edit, base)) = self.state.pending_edit.take() {
            self.turn(Duration::ZERO)?;
            if self.state.field.before() == Some(base.as_str()) {
                let serial = self.state.serial;
                let delete = u32::try_from(edit.delete).unwrap_or(u32::MAX);
                self.method.delete_surrounding_text(delete, 0);
                self.method.commit_string(edit.insert);
                self.method.commit(serial);
            }
        }
        self.connection.flush()?;
        Ok(())
    }

    /// Dispatches what has arrived, waiting up to `timeout` for more.
    fn turn(&mut self, timeout: Duration) -> Result<()> {
        self.queue.dispatch_pending(&mut self.state)?;
        self.connection.flush()?;
        if let Some(guard) = self.queue.prepare_read()
            && readable(guard.connection_fd(), timeout)
            // Events another reader took are dispatched on the next turn.
            && let Err(error) = guard.read()
            && !matches!(&error, wayland_client::backend::WaylandError::Io(io) if io.kind() == std::io::ErrorKind::WouldBlock)
        {
            return Err(error.into());
        }
        self.queue.dispatch_pending(&mut self.state)?;
        Ok(())
    }

    fn save(&mut self) {
        if !self.state.dirty {
            return;
        }
        if let Some(path) = &self.path
            && let Err(error) = self.state.learned.save(path)
        {
            tracing::warn!(%error, "autocorrect could not save its learned words");
            return;
        }
        self.state.dirty = false;
    }

    /// Gives the seat's input method back.
    fn end(&mut self) {
        self.save();
        self.method.destroy();
        self.manager.destroy();
        let _ = self.connection.flush();
        tracing::info!("autocorrect released the input method");
    }
}

/// Waits for the Wayland socket to be readable, as `remote_desktop/keymap.rs`.
fn readable(fd: std::os::fd::BorrowedFd<'_>, timeout: Duration) -> bool {
    let mut poll = libc::pollfd {
        fd: fd.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    let timeout = timeout.as_millis().min(i32::MAX as u128) as libc::c_int;
    // SAFETY: `poll` reads one valid `pollfd` for the Wayland socket fd.
    unsafe { libc::poll(&mut poll, 1, timeout) > 0 }
}

/// The input method's double-buffered state and what autocorrect knows.
struct State {
    corrector: Corrector,
    learned: Learned,
    field: Field,
    /// The number of `done` events seen; the serial `commit` takes.
    serial: u32,
    active: bool,
    /// The focused field takes ordinary text.
    writable: bool,
    unavailable: bool,
    pending_active: Option<bool>,
    pending_text: Option<(String, u32, u32)>,
    pending_content: Option<bool>,
    /// Learned words changed since the last save.
    dirty: bool,
    /// The fix a `done` asked for, with the text before the cursor it was
    /// made for, until it is sent.
    pending_edit: Option<(field::Edit, String)>,
    /// Labs › Smarter autocorrect, once loaded.
    language: Arc<Mutex<Option<LanguageModel>>>,
}

impl State {
    fn new(
        corrector: Corrector,
        learned: Learned,
        language: Arc<Mutex<Option<LanguageModel>>>,
    ) -> Self {
        Self {
            corrector,
            learned,
            field: Field::default(),
            serial: 0,
            active: false,
            writable: true,
            unavailable: false,
            pending_active: None,
            pending_text: None,
            pending_content: None,
            dirty: false,
            pending_edit: None,
            language,
        }
    }

    /// Applies one `done`, and returns the edit to commit with its serial.
    fn done(&mut self) -> Option<field::Edit> {
        self.serial = self.serial.wrapping_add(1);
        if let Some(active) = self.pending_active.take() {
            self.active = active;
            self.writable = true;
            self.field.reset();
        }
        if let Some(writable) = self.pending_content.take() {
            self.writable = writable;
        }
        let text = self.pending_text.take();
        if !self.active || !self.writable {
            self.field.reset();
            return None;
        }
        let (text, cursor, anchor) = text?;
        let cursor = cursor as usize;
        let before =
            (cursor == anchor as usize && text.is_char_boundary(cursor)).then(|| &text[..cursor]);
        let generation = self.learned.generation();
        let mut language = self.language.lock().unwrap();
        let scorer = language
            .as_mut()
            .map(|model| model as &mut dyn correct::Scorer);
        let edit = self
            .field
            .changed(before, &self.corrector, &mut self.learned, scorer);
        self.dirty |= generation != self.learned.generation();
        edit
    }
}

/// Whether a field of this kind takes autocorrect.
fn writable(hint: WEnum<ContentHint>, purpose: WEnum<ContentPurpose>) -> bool {
    let hint = match hint {
        WEnum::Value(hint) => hint,
        WEnum::Unknown(_) => ContentHint::empty(),
    };
    !hint.intersects(ContentHint::SensitiveData | ContentHint::HiddenText)
        && matches!(
            purpose,
            WEnum::Value(ContentPurpose::Normal | ContentPurpose::Alpha | ContentPurpose::Name)
        )
}

impl Dispatch<ZwpInputMethodV2, ()> for State {
    fn event(
        state: &mut Self,
        _: &ZwpInputMethodV2,
        event: InputMethodEvent,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            InputMethodEvent::Activate => state.pending_active = Some(true),
            InputMethodEvent::Deactivate => state.pending_active = Some(false),
            InputMethodEvent::SurroundingText {
                text,
                cursor,
                anchor,
            } => state.pending_text = Some((text, cursor, anchor)),
            InputMethodEvent::ContentType { hint, purpose } => {
                state.pending_content = Some(writable(hint, purpose));
            }
            InputMethodEvent::Done => {
                // An unsent fix waits through repeated reports of the same
                // text; `pump` drops it once the text moved on.
                if let Some(edit) = state.done()
                    && let Some(base) = state.field.before()
                {
                    state.pending_edit = Some((edit, base.to_owned()));
                }
            }
            InputMethodEvent::Unavailable => state.unavailable = true,
            InputMethodEvent::TextChangeCause { .. } => {}
        }
    }
}

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for State {
    fn event(
        _: &mut Self,
        _: &wl_registry::WlRegistry,
        _: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<wl_seat::WlSeat, ()> for State {
    fn event(
        _: &mut Self,
        _: &wl_seat::WlSeat,
        _: wl_seat::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ZwpInputMethodManagerV2, ()> for State {
    fn event(
        _: &mut Self,
        _: &ZwpInputMethodManagerV2,
        _: <ZwpInputMethodManagerV2 as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_ordinary_text_fields_take_autocorrect() {
        let ok = |hint, purpose| writable(WEnum::Value(hint), WEnum::Value(purpose));
        assert!(ok(ContentHint::Spellcheck, ContentPurpose::Normal));
        assert!(ok(ContentHint::empty(), ContentPurpose::Name));
        assert!(!ok(ContentHint::empty(), ContentPurpose::Password));
        assert!(!ok(ContentHint::empty(), ContentPurpose::Terminal));
        assert!(!ok(ContentHint::empty(), ContentPurpose::Url));
        assert!(!ok(ContentHint::SensitiveData, ContentPurpose::Normal));
    }

    /// Holds the input method on `WAYLAND_DISPLAY` for
    /// `HEX_AUTOCORRECT_LIVE_SECS` seconds, for an end-to-end check against a
    /// real compositor and text field.
    #[test]
    #[ignore = "needs a Wayland compositor and a text field to type into"]
    fn live_compositor() {
        let seconds = std::env::var("HEX_AUTOCORRECT_LIVE_SECS")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(10);
        let mut owner = Owner::connect(None, Arc::default()).unwrap();
        let end = Instant::now() + Duration::from_secs(seconds);
        while Instant::now() < end {
            owner.pump(TICK).unwrap();
            assert!(
                !owner.state.unavailable,
                "another input method holds the seat"
            );
        }
        owner.end();
    }

    #[test]
    fn a_done_applies_activation_and_fixes_the_finished_word() {
        let mut state = State::new(Corrector::built_in(), Learned::default(), Arc::default());
        state.pending_text = Some(("teh".into(), 3, 3));
        assert_eq!(state.done(), None, "inactive");
        state.pending_active = Some(true);
        state.pending_text = Some(("so teh".into(), 6, 6));
        assert_eq!(state.done(), None);
        state.pending_text = Some(("so teh ".into(), 7, 7));
        let edit = state.done().unwrap();
        assert_eq!((edit.delete, edit.insert.as_str()), (4, "the "));
        assert_eq!(state.serial, 3);
    }
}
