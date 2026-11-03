//! `tern gateway pair`.

use std::process::ExitCode;

use tern_core::{Config, TernHome};
use tern_store::Store;

const CODE_TTL_MS: i64 = 10 * 60 * 1000;
/// No 0/O or 1/I, so the code survives being read off a screen.
const ALPHABET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";

pub fn pair(home: &TernHome) -> anyhow::Result<ExitCode> {
    let (config, _) = Config::load(&home.config_path())?;
    let store = Store::open(&home.db_path())?;
    // UUIDv4 bytes come from the OS's CSPRNG; 32 symbols divide 256 evenly,
    // so taking each byte mod 32 is unbiased.
    let bytes = *uuid::Uuid::new_v4().as_bytes();
    let code: String = bytes[..8]
        .iter()
        .map(|b| ALPHABET[usize::from(*b) % ALPHABET.len()] as char)
        .collect();
    store.create_pairing_code(&code, CODE_TTL_MS)?;
    println!("{code}");
    eprintln!("Send this code to your Telegram bot within 10 minutes to pair your account.");
    if !config.gateway.telegram.as_ref().is_some_and(|t| t.enabled) {
        eprintln!(
            "note: the Telegram gateway isn't enabled; set [gateway.telegram] enabled = true and bot_token_secret in config.toml, then run `tern daemon`."
        );
    }
    Ok(ExitCode::SUCCESS)
}
