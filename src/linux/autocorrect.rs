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
mod learned;
mod model;
mod snippets;

use std::os::fd::AsRawFd;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock, mpsc};
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
use self::learned::Learned;
use crate::linux::live_typing::protocol::zwp_input_method_manager_v2::ZwpInputMethodManagerV2;
use crate::linux::live_typing::protocol::zwp_input_method_v2::{
    Event as InputMethodEvent, ZwpInputMethodV2,
};

/// How often the thread looks at the lab's switch and Hex's shutdown.
const TICK: Duration = Duration::from_millis(100);
/// How long after the seat was unavailable Hex asks for it again.
const RETRY: Duration = Duration::from_secs(30);
/// Learned words are written at most this often.
const SAVE_EVERY: Duration = Duration::from_secs(5);

#[derive(Default)]
struct Preferences {
    excluded_apps: Vec<String>,
    snippets: bool,
}

static PREFERENCES: Mutex<Preferences> = Mutex::new(Preferences {
    excluded_apps: Vec::new(),
    snippets: false,
});
static DICTIONARY: OnceLock<(Option<PathBuf>, Mutex<Learned>)> = OnceLock::new();
static REVISION: AtomicU64 = AtomicU64::new(0);
static LAST_CORRECTION: Mutex<Option<(String, String)>> = Mutex::new(None);
static UNDO: OnceLock<mpsc::SyncSender<Undo>> = OnceLock::new();

struct Undo {
    deadline: Instant,
    reply: mpsc::Sender<Result<bool, String>>,
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|error| error.into_inner())
}

pub(crate) fn apply(settings: &crate::app_settings::LabsSettings) {
    *lock(&PREFERENCES) = Preferences {
        excluded_apps: settings.autocorrect_excluded_apps.clone(),
        snippets: settings.autocorrect_snippets,
    };
}

fn dictionary() -> &'static (Option<PathBuf>, Mutex<Learned>) {
    DICTIONARY.get_or_init(|| {
        let path = crate::app_paths::support_dir()
            .ok()
            .map(|dir| dir.join("autocorrect.json"));
        let learned = path.as_deref().map(Learned::load).unwrap_or_default();
        (path, Mutex::new(learned))
    })
}

pub(crate) fn entries(query: &str) -> Vec<(bool, String, String)> {
    lock(&dictionary().1).entries(&query.trim().to_lowercase())
}

pub(crate) fn revision() -> u64 {
    REVISION.load(Ordering::Relaxed)
}

pub(crate) fn last_correction() -> Option<(String, String)> {
    lock(&LAST_CORRECTION).clone()
}

pub(crate) fn forget(typo: bool, key: &str) -> std::io::Result<()> {
    let (path, learned) = dictionary();
    let mut learned = lock(learned);
    learned.forget(typo, key);
    REVISION.fetch_add(1, Ordering::Relaxed);
    if let Some(path) = path {
        learned.save(path)?;
    }
    Ok(())
}

pub(crate) fn undo() -> Result<bool, String> {
    let sender = UNDO
        .get()
        .filter(|_| crate::labs::autocorrect())
        .ok_or("Enable Labs › Autocorrect in a Wayland session first")?;
    let (reply, response) = mpsc::channel();
    let timeout = Duration::from_secs(1);
    sender
        .try_send(Undo {
            deadline: Instant::now() + timeout,
            reply,
        })
        .map_err(|_| "Autocorrect is busy")?;
    response
        .recv_timeout(timeout)
        .map_err(|_| "Autocorrect did not respond")?
}

/// Starts the autocorrect thread on Wayland; it idles while the lab is off.
pub(crate) fn start(shutdown: &'static AtomicBool) {
    if !crate::linux::session::LinuxSession::detect().is_wayland() {
        return;
    }
    let (sender, requests) = mpsc::sync_channel(1);
    if UNDO.set(sender).is_err() {
        return;
    }
    if let Err(error) = thread::Builder::new()
        .name("hex-autocorrect".into())
        .spawn(move || run(shutdown, requests))
    {
        tracing::info!(%error, "autocorrect is unavailable");
    }
}

fn run(shutdown: &AtomicBool, requests: mpsc::Receiver<Undo>) {
    let mut owner: Option<Owner> = None;
    let mut retry_at = Instant::now();
    let mut saved_at = Instant::now();
    while !shutdown.load(Ordering::Acquire) {
        let on = crate::labs::autocorrect();
        if on && owner.is_none() && Instant::now() >= retry_at {
            match Owner::connect() {
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
            if let Ok(request) = requests.recv_timeout(TICK) {
                let _ = request
                    .reply
                    .send(Err("Autocorrect does not hold the input method".into()));
            }
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
        if let Ok(request) = requests.try_recv() {
            let result = current
                .undo(request.deadline)
                .map_err(|error| error.to_string());
            let _ = request.reply.send(result);
        }
        if saved_at.elapsed() >= SAVE_EVERY {
            current.save();
            saved_at = Instant::now();
        }
    }
    if let Some(mut ended) = owner {
        ended.end();
    }
}

struct Owner {
    connection: Connection,
    queue: EventQueue<State>,
    manager: ZwpInputMethodManagerV2,
    method: ZwpInputMethodV2,
    state: State,
    saved_generation: u64,
}

impl Owner {
    fn connect() -> Result<Self> {
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
        let mut state = State::new(Corrector::built_in());
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
            saved_generation: 0,
        })
    }

    /// Handles the compositor's events for up to `timeout`, then sends the
    /// fixes they asked for.
    fn pump(&mut self, timeout: Duration) -> Result<()> {
        self.turn(timeout)?;
        // A fix is sent only while no newer text has arrived: once you typed
        // on, deleting the word's bytes before the cursor would hit your new
        // letters.
        while let Some((edit, base, field_epoch)) = self.state.pending_edit.take() {
            self.turn(Duration::ZERO)?;
            if self.state.field_epoch == field_epoch
                && self.state.field.before() == Some(base.as_str())
                && self.state.allowed(&lock(&PREFERENCES))
            {
                self.commit(edit);
                if let Some((original, fix)) = self.state.field.correction() {
                    *lock(&LAST_CORRECTION) = Some((original.to_owned(), fix.to_owned()));
                    REVISION.fetch_add(1, Ordering::Relaxed);
                }
            }
        }
        self.connection.flush()?;
        Ok(())
    }

    fn commit(&self, edit: field::Edit) {
        let delete = u32::try_from(edit.delete).unwrap_or(u32::MAX);
        self.method.delete_surrounding_text(delete, 0);
        self.method.commit_string(edit.insert);
        self.method.commit(self.state.serial);
    }

    fn undo(&mut self, deadline: Instant) -> Result<bool> {
        self.turn(Duration::ZERO)?;
        if Instant::now() >= deadline
            || !crate::labs::autocorrect()
            || !self.state.allowed(&lock(&PREFERENCES))
        {
            return Ok(false);
        }
        if let Some(edit) = self.state.field.undo() {
            self.commit(edit);
            self.connection.flush()?;
            return Ok(true);
        }
        Ok(false)
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
        let (path, learned) = dictionary();
        let learned = lock(learned);
        let generation = learned.generation();
        if self.saved_generation == generation {
            return;
        }
        if let Some(path) = path
            && let Err(error) = learned.save(path)
        {
            tracing::warn!(%error, "autocorrect could not save its learned words");
            return;
        }
        self.saved_generation = generation;
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
    application: Option<String>,
    field: Field,
    field_epoch: u64,
    /// The number of `done` events seen; the serial `commit` takes.
    serial: u32,
    active: bool,
    /// The focused field takes ordinary text.
    writable: bool,
    unavailable: bool,
    pending_active: Option<bool>,
    pending_text: Option<(String, u32, u32)>,
    pending_content: Option<bool>,
    /// The fix a `done` asked for, with the text before the cursor it was
    /// made for, until it is sent.
    pending_edit: Option<(field::Edit, String, u64)>,
}

impl State {
    fn new(corrector: Corrector) -> Self {
        Self {
            corrector,
            application: None,
            field: Field::default(),
            field_epoch: 0,
            serial: 0,
            active: false,
            writable: true,
            unavailable: false,
            pending_active: None,
            pending_text: None,
            pending_content: None,
            pending_edit: None,
        }
    }

    fn reset_field(&mut self) {
        self.field_epoch = self.field_epoch.wrapping_add(1);
        self.field.reset();
        self.pending_edit = None;
    }

    fn allowed(&self, prefs: &Preferences) -> bool {
        self.active
            && self.writable
            && !self.unavailable
            && application_allowed(&prefs.excluded_apps, self.application.as_deref())
    }

    /// Applies one `done`, and returns the edit to commit with its serial.
    fn done(
        &mut self,
        prefs: &Preferences,
        learned: &mut Learned,
        application: impl FnOnce() -> Option<String>,
    ) -> Option<field::Edit> {
        self.serial = self.serial.wrapping_add(1);
        if let Some(active) = self.pending_active.take() {
            self.active = active;
            self.writable = true;
            self.reset_field();
            self.application = active.then(application).flatten();
        }
        if let Some(writable) = self.pending_content.take() {
            self.writable = writable;
        }
        let text = self.pending_text.take();
        if !self.allowed(prefs) {
            self.reset_field();
            return None;
        }
        let (text, cursor, anchor) = text?;
        let cursor = cursor as usize;
        let before =
            (cursor == anchor as usize && text.is_char_boundary(cursor)).then(|| &text[..cursor]);
        let Some(before) = before else {
            self.reset_field();
            return None;
        };
        let revision = learned.generation();
        let edit = self
            .field
            .changed(Some(before), &self.corrector, learned, prefs.snippets);
        if learned.generation() != revision {
            REVISION.fetch_add(1, Ordering::Relaxed);
        }
        edit
    }
}

fn application_allowed(excluded: &[String], application: Option<&str>) -> bool {
    application != Some(super::portal::APP_ID)
        && (excluded.is_empty()
            || application
                .is_some_and(|name| !excluded.iter().any(|app| app.eq_ignore_ascii_case(name))))
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
                // Resolve the app only on activation, outside the settings and
                // dictionary locks.
                let application = (state.pending_active == Some(true))
                    .then(crate::linux::context::focused_wayland_application)
                    .flatten();
                if let Some(edit) =
                    state.done(&lock(&PREFERENCES), &mut lock(&dictionary().1), || {
                        application
                    })
                    && let Some(base) = state.field.before()
                {
                    state.pending_edit = Some((edit, base.to_owned(), state.field_epoch));
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
        let _ = DICTIONARY.set((None, Mutex::new(Learned::default())));
        let mut owner = Owner::connect().unwrap();
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
    fn an_excluded_app_neither_changes_text_nor_learns() {
        let mut state = State::new(Corrector::built_in());
        let mut learned = Learned::default();
        let mut prefs = Preferences {
            excluded_apps: vec!["Editor".into()],
            snippets: true,
        };
        let app = || Some("editor".into());
        state.pending_active = Some(true);
        state.pending_text = Some((";lg".into(), 3, 3));
        assert_eq!(state.done(&prefs, &mut learned, app), None);
        state.pending_text = Some((";lg ".into(), 4, 4));
        assert_eq!(state.done(&prefs, &mut learned, app), None);
        assert_eq!(state.field.before(), None);
        assert_eq!(learned.generation(), 0);

        prefs.excluded_apps.clear();
        state.pending_text = Some((";lg".into(), 3, 3));
        assert_eq!(state.done(&prefs, &mut learned, app), None);
        state.pending_text = Some((";lg ".into(), 4, 4));
        assert_eq!(
            state.done(&prefs, &mut learned, app).unwrap().insert,
            "Liebe Grüße "
        );
    }

    #[test]
    fn a_done_applies_activation_and_fixes_the_finished_word() {
        let mut state = State::new(Corrector::built_in());
        let mut learned = Learned::default();
        let prefs = Preferences::default();
        let app = || Some("test".into());
        state.pending_text = Some(("teh".into(), 3, 3));
        assert_eq!(state.done(&prefs, &mut learned, app), None, "inactive");
        state.pending_active = Some(true);
        state.pending_text = Some(("so teh".into(), 6, 6));
        assert_eq!(state.done(&prefs, &mut learned, app), None);
        state.pending_text = Some(("so teh ".into(), 7, 7));
        let edit = state.done(&prefs, &mut learned, app).unwrap();
        assert_eq!((edit.delete, edit.insert.as_str()), (4, "the "));
        assert_eq!(state.serial, 3);
    }
}
