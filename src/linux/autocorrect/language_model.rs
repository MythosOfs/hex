//! Labs › Smarter autocorrect: Gemma 3 270M, run through the
//! system's llama.cpp (`libllama.so.1`, loaded at run time, so Hex never links
//! it and works unchanged without it). The model is downloaded once, pinned by
//! size and SHA-256, into Hex's models folder. It only scores: how unlikely a
//! continuation is after the text so far (`Scorer`), which `correct.rs` uses
//! to choose between a typo's readings.

use std::ffi::{CStr, CString, c_char, c_void};
use std::fs;
use std::path::PathBuf;
use std::sync::OnceLock;
use std::sync::atomic::AtomicBool;

use color_eyre::eyre::{Result, bail, eyre};

use super::correct::Scorer;

const URL: &str = "https://huggingface.co/ggml-org/gemma-3-270m-GGUF/resolve/360e988e4c99209d62d171ca1a46aceb4b224791/gemma-3-270m-Q8_0.gguf";
const FILE: &str = "gemma-3-270m-Q8_0.gguf";
const BYTES: u64 = 291_543_744;
const SHA256: &str = "e00cf79514204dfe2f4d6943f277ffea7fd8a4c8e955b4b7a869cfc157694881";
/// Tokens the context window holds; a review needs a sentence and a word.
const WINDOW: u32 = 512;

/// `llama_model_params` and `llama_context_params`, passed whole and never
/// read except the fields named below. Both are larger than 16 bytes, so
/// x86-64 and AArch64 pass them in memory, and room to spare keeps a newer
/// libllama's larger struct safe.
#[repr(C, align(8))]
#[derive(Clone, Copy)]
struct Params([u8; 1024]);

#[repr(C)]
#[derive(Clone, Copy)]
struct Batch {
    n_tokens: i32,
    token: *mut i32,
    embd: *mut f32,
    pos: *mut i32,
    n_seq_id: *mut i32,
    seq_id: *mut *mut i32,
    logits: *mut i8,
}

type Log = unsafe extern "C" fn(i32, *const c_char, *mut c_void);

macro_rules! library {
    ($name:ident { $($f:ident: fn($($arg:ty),*) $(-> $ret:ty)?;)* }) => {
        struct $name {
            $($f: unsafe extern "C" fn($($arg),*) $(-> $ret)?,)*
        }

        impl $name {
            /// # Safety
            /// `handle` is a `dlopen` handle of a library exporting these C functions.
            unsafe fn load(handle: *mut c_void) -> Result<Self> {
                Ok(Self {
                    $($f: {
                        let name = concat!(stringify!($f), "\0");
                        let symbol = unsafe { libc::dlsym(handle, name.as_ptr().cast()) };
                        if symbol.is_null() {
                            bail!("this libllama has no {}", stringify!($f));
                        }
                        unsafe { std::mem::transmute::<*mut c_void, unsafe extern "C" fn($($arg),*) $(-> $ret)?>(symbol) }
                    },)*
                })
            }
        }
    };
}

library!(Llama {
    llama_backend_init: fn();
    llama_log_set: fn(Log, *mut c_void);
    llama_model_default_params: fn() -> Params;
    llama_model_load_from_file: fn(*const c_char, Params) -> *mut c_void;
    llama_model_free: fn(*mut c_void);
    llama_model_get_vocab: fn(*mut c_void) -> *const c_void;
    llama_vocab_n_tokens: fn(*const c_void) -> i32;
    llama_context_default_params: fn() -> Params;
    llama_init_from_model: fn(*mut c_void, Params) -> *mut c_void;
    llama_free: fn(*mut c_void);
    llama_get_memory: fn(*mut c_void) -> *mut c_void;
    llama_memory_clear: fn(*mut c_void, bool);
    llama_memory_seq_rm: fn(*mut c_void, i32, i32, i32) -> bool;
    llama_batch_init: fn(i32, i32, i32) -> Batch;
    llama_batch_free: fn(Batch);
    llama_decode: fn(*mut c_void, Batch) -> i32;
    llama_get_logits_ith: fn(*mut c_void, i32) -> *mut f32;
    llama_tokenize: fn(*const c_void, *const c_char, i32, *mut i32, i32, bool, bool) -> i32;
});

unsafe extern "C" fn quiet(_: i32, _: *const c_char, _: *mut c_void) {}

/// The system's libllama, opened and initialised once per process.
fn llama() -> Result<&'static Llama> {
    static LLAMA: OnceLock<std::result::Result<Llama, String>> = OnceLock::new();
    LLAMA
        .get_or_init(|| {
            let open = |soname: &CStr| unsafe {
                libc::dlopen(soname.as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL)
            };
            let mut handle = open(c"libllama.so.1");
            if handle.is_null() {
                handle = open(c"libllama.so");
            }
            if handle.is_null() {
                return Err("llama.cpp isn't installed (libllama.so.1)".into());
            }
            // SAFETY: `handle` is libllama, which exports these functions.
            let llama = unsafe { Llama::load(handle) }.map_err(|error| error.to_string())?;
            unsafe {
                (llama.llama_log_set)(quiet, std::ptr::null_mut());
                (llama.llama_backend_init)();
            }
            Ok(llama)
        })
        .as_ref()
        .map_err(|why| eyre!("{why}"))
}

/// Downloads the pinned model once (resuming a partial download) and
/// returns its path.
pub(crate) fn fetch(canceled: &AtomicBool) -> Result<PathBuf> {
    let directory = crate::app_paths::models_dir()?.join("autocorrect");
    let path = directory.join(FILE);
    if fs::metadata(&path).is_ok_and(|meta| meta.len() == BYTES) {
        return Ok(path);
    }
    fs::create_dir_all(&directory)?;
    let partial = directory.join(format!("{FILE}.partial"));
    crate::net::download(
        &crate::net::Download {
            url: URL,
            user_agent: &format!("hex/{}", crate::VERSION),
            size: Some(BYTES),
            canceled: Some(canceled),
        },
        &partial,
        |_| {},
    )?;
    let digest = crate::transcription::models::sha256_hex(&partial, Some(canceled))?;
    if digest != SHA256 {
        let _ = fs::remove_file(&partial);
        bail!("the downloaded language model didn't match its checksum");
    }
    crate::durable::sync_file(&partial)?;
    fs::rename(&partial, &path)?;
    crate::durable::sync_directory(&directory)?;
    Ok(path)
}

/// A loaded model and its context, scoring on one sequence.
pub(crate) struct LanguageModel {
    llama: &'static Llama,
    model: *mut c_void,
    context: *mut c_void,
    memory: *mut c_void,
    vocab: *const c_void,
    vocab_size: usize,
    batch: Batch,
    /// The context text in the cache, its token count, and the logits after it.
    cached: Option<(String, i32, Vec<f32>)>,
}

// SAFETY: libllama's model and context may be used from any thread, one at a
// time; `LanguageModel` owns them and needs `&mut` to use them.
unsafe impl Send for LanguageModel {}

impl LanguageModel {
    pub(crate) fn load(path: &std::path::Path) -> Result<Self> {
        let llama = llama()?;
        let path = CString::new(path.as_os_str().as_encoded_bytes())?;
        unsafe {
            // Every layer on the GPU when there is one, as llama.cpp defaults.
            let model = (llama.llama_model_load_from_file)(
                path.as_ptr(),
                (llama.llama_model_default_params)(),
            );
            if model.is_null() {
                bail!("llama.cpp could not load the language model");
            }
            let mut params = (llama.llama_context_default_params)();
            // n_ctx, n_batch, n_ubatch, n_seq_max lead the struct and have
            // since 2023; anything else there is a libllama Hex doesn't know.
            let field = |params: &Params, at: usize| {
                u32::from_ne_bytes(params.0[at..at + 4].try_into().unwrap())
            };
            if !(field(&params, 12) == 1 && (1..=field(&params, 4)).contains(&field(&params, 8))) {
                (llama.llama_model_free)(model);
                bail!("this libllama's context parameters aren't the ones Hex knows");
            }
            params.0[0..4].copy_from_slice(&WINDOW.to_ne_bytes());
            params.0[4..8].copy_from_slice(&WINDOW.to_ne_bytes());
            let ubatch = field(&params, 8).min(WINDOW);
            params.0[8..12].copy_from_slice(&ubatch.to_ne_bytes());
            let context = (llama.llama_init_from_model)(model, params);
            if context.is_null() {
                (llama.llama_model_free)(model);
                bail!("llama.cpp could not set up the language model");
            }
            let vocab = (llama.llama_model_get_vocab)(model);
            Ok(Self {
                llama,
                model,
                context,
                memory: (llama.llama_get_memory)(context),
                vocab,
                vocab_size: (llama.llama_vocab_n_tokens)(vocab) as usize,
                batch: (llama.llama_batch_init)(WINDOW as i32, 0, 1),
                cached: None,
            })
        }
    }

    fn tokenize(&self, text: &str, start: bool) -> Vec<i32> {
        let length = i32::try_from(text.len()).unwrap_or(i32::MAX);
        let mut tokens = vec![0; text.len() + 2];
        let count = unsafe {
            (self.llama.llama_tokenize)(
                self.vocab,
                text.as_ptr().cast(),
                length,
                tokens.as_mut_ptr(),
                tokens.len() as i32,
                start,
                false,
            )
        };
        tokens.truncate(count.max(0) as usize);
        tokens
    }

    /// Decodes `tokens` at positions from `start`, with logits for the last
    /// one or for all.
    fn decode(&mut self, tokens: &[i32], start: i32, all: bool) -> bool {
        let batch = self.batch;
        unsafe {
            for (i, token) in tokens.iter().enumerate() {
                *batch.token.add(i) = *token;
                *batch.pos.add(i) = start + i as i32;
                *batch.n_seq_id.add(i) = 1;
                **batch.seq_id.add(i) = 0;
                *batch.logits.add(i) = i8::from(all || i + 1 == tokens.len());
            }
            let batch = Batch {
                n_tokens: tokens.len() as i32,
                ..batch
            };
            (self.llama.llama_decode)(self.context, batch) == 0
        }
    }

    fn logits(&self, i: i32) -> &[f32] {
        unsafe {
            std::slice::from_raw_parts(
                (self.llama.llama_get_logits_ith)(self.context, i),
                self.vocab_size,
            )
        }
    }
}

/// -ln P(token) under `logits`.
fn nats(logits: &[f32], token: i32) -> f32 {
    let top = logits.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    let sum: f32 = logits.iter().map(|logit| (logit - top).exp()).sum();
    sum.ln() + top - logits[token as usize]
}

impl Scorer for LanguageModel {
    fn score(&mut self, context: &str, continuation: &str) -> Option<f32> {
        if self
            .cached
            .as_ref()
            .is_none_or(|(text, ..)| text != context)
        {
            self.cached = None;
            unsafe { (self.llama.llama_memory_clear)(self.memory, true) };
            let mut tokens = self.tokenize(context, true);
            // A model without a start token needs something to stand on.
            if tokens.is_empty() {
                tokens = self.tokenize("\n", false);
            }
            if tokens.len() as u32 >= WINDOW / 2 || !self.decode(&tokens, 0, false) {
                return None;
            }
            let logits = self.logits(-1).to_vec();
            self.cached = Some((context.to_owned(), tokens.len() as i32, logits));
        } else if let Some((_, length, _)) = &self.cached {
            unsafe { (self.llama.llama_memory_seq_rm)(self.memory, 0, *length, -1) };
        }
        let (_, length, logits) = self.cached.as_ref()?;
        let length = *length;
        let spaced;
        let continuation = if context.is_empty() {
            continuation
        } else {
            spaced = format!(" {continuation}");
            &spaced
        };
        let tokens = self.tokenize(continuation, false);
        let mut total = nats(logits, *tokens.first()?);
        if tokens.len() > 1 {
            if tokens.len() as u32 >= WINDOW / 2 || !self.decode(&tokens, length, true) {
                return None;
            }
            for (i, token) in tokens.iter().enumerate().skip(1) {
                total += nats(self.logits(i as i32 - 1), *token);
            }
        }
        Some(total)
    }
}

impl Drop for LanguageModel {
    fn drop(&mut self) {
        unsafe {
            (self.llama.llama_batch_free)(self.batch);
            (self.llama.llama_free)(self.context);
            (self.llama.llama_model_free)(self.model);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Scores with the real model, for a check against the experiment.
    #[test]
    #[ignore = "needs llama.cpp and downloads a 290 MB model"]
    fn scores_like_the_experiment() {
        let path = fetch(&AtomicBool::new(false)).unwrap();
        let mut model = LanguageModel::load(&path).unwrap();
        let good = model.score("Das ist", "ein guter Tag").unwrap();
        let bad = model.score("Das ist", "eim guter Tag").unwrap();
        assert!(bad - good > 4.0, "{good} {bad}");
        assert!(model.score("", "Hallo").is_some());
    }
}
