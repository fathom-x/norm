"""Generate a Python-format owallet DB fixture for the Rust port to read.

This script reproduces the exact wire format used by
`overpay/owallet/wallet_mcp/db.py` (the original Python implementation):

- SQLite schema and ALTER TABLE migrations from db.py:103-152, 70-83
- PBKDF2-HMAC-SHA256 verify hash with count=1 (db.py:176)
- PBKDF2-HMAC-SHA256 AES key derivation with count=600_000 (db.py:213)
- AES-256-GCM with 16-byte nonces, layout = ciphertext || 16-byte tag (db.py:243-249)

The output `python_v0_1_0.db` is committed in this directory and read back by
the Rust integration test `python_compat.rs` to prove the Rust port can
unlock and decrypt a database produced by the Python implementation.

Re-run after touching the schema:

    python3 generate_python_v0_1_0.py
"""

import hashlib
import os
import secrets
import sqlite3
import time
from pathlib import Path

from cryptography.hazmat.primitives.ciphers.aead import AESGCM

# Stable, well-known values so the test asserts exact equality.
PASSWORD = "fixture-pw"
NPUB = "npub1fixturefixturefixture"
MNEMONIC = (
    "abandon abandon abandon abandon abandon abandon "
    "abandon abandon abandon abandon abandon about"
)
ADDRESS = "0x1234567890abcdef1234567890abcdef12345678"

# Deterministic, non-random salt and nonce so the fixture is byte-stable
# across regenerations. (Real DBs use OsRng for these.)
SALT = bytes.fromhex("00112233445566778899aabbccddeeff" * 2)  # 32 bytes
NONCE = bytes.fromhex("aabbccddeeff00112233445566778899")  # 16 bytes

OUT_PATH = Path(__file__).parent / "python_v0_1_0.db"

SCHEMA = """
CREATE TABLE IF NOT EXISTS settings (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS wallets (
    npub           TEXT PRIMARY KEY,
    encrypted_seed BLOB NOT NULL,
    nonce          BLOB NOT NULL,
    created_at     INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS tokens (
    npub            TEXT NOT NULL,
    host            TEXT NOT NULL,
    encrypted_token BLOB NOT NULL,
    nonce           BLOB NOT NULL,
    token_name      TEXT,
    stored_at       INTEGER NOT NULL,
    PRIMARY KEY (npub, host)
);

CREATE TABLE IF NOT EXISTS oauth_clients (
    client_id     TEXT PRIMARY KEY,
    client_secret TEXT,
    redirect_uris TEXT NOT NULL,
    grant_types   TEXT NOT NULL,
    registered_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS auth_codes (
    code                             TEXT PRIMARY KEY,
    client_id                        TEXT NOT NULL,
    scopes                           TEXT NOT NULL,
    code_challenge                   TEXT NOT NULL,
    redirect_uri                     TEXT NOT NULL,
    redirect_uri_provided_explicitly INTEGER NOT NULL,
    expires_at                       REAL NOT NULL
);

CREATE TABLE IF NOT EXISTS access_tokens (
    token      TEXT PRIMARY KEY,
    client_id  TEXT NOT NULL,
    scopes     TEXT NOT NULL,
    expires_at INTEGER
);
"""

# Migrations applied by db.py on every open. The fixture exercises them by
# only including the base schema above, then running these ALTERs.
MIGRATIONS = [
    "ALTER TABLE oauth_clients ADD COLUMN scope TEXT",
    "ALTER TABLE oauth_clients ADD COLUMN token_endpoint_auth_method TEXT",
    "ALTER TABLE wallets ADD COLUMN last_accessed INTEGER",
    "ALTER TABLE auth_codes ADD COLUMN npub TEXT",
    "ALTER TABLE access_tokens ADD COLUMN npub TEXT",
    "ALTER TABLE wallets ADD COLUMN wallet_password_hash TEXT",
    "ALTER TABLE wallets ADD COLUMN address TEXT",
    "ALTER TABLE wallets ADD COLUMN overpay_username TEXT",
]


def pbkdf2_hex(password: str, salt: bytes, count: int) -> str:
    return hashlib.pbkdf2_hmac("sha256", password.encode(), salt, count, 32).hex()


def encrypt(key: bytes, plaintext: bytes, nonce: bytes) -> bytes:
    """AES-256-GCM, 16-byte nonce, layout = ciphertext || 16-byte tag."""
    return AESGCM(key).encrypt(nonce, plaintext, associated_data=None)


def main() -> None:
    if OUT_PATH.exists():
        OUT_PATH.unlink()

    conn = sqlite3.connect(str(OUT_PATH))
    conn.executescript(SCHEMA)
    for ddl in MIGRATIONS:
        conn.execute(ddl)

    # settings
    verify = pbkdf2_hex(PASSWORD, SALT, 1)
    conn.execute(
        "INSERT INTO settings(key, value) VALUES('db_salt', ?)",
        (SALT.hex(),),
    )
    conn.execute(
        "INSERT INTO settings(key, value) VALUES('password_hash', ?)",
        (verify,),
    )

    # wallet
    aes_key = bytes.fromhex(pbkdf2_hex(PASSWORD, SALT, 600_000))
    ct_with_tag = encrypt(aes_key, MNEMONIC.encode(), NONCE)
    conn.execute(
        """INSERT INTO wallets(npub, encrypted_seed, nonce, created_at, address)
           VALUES(?, ?, ?, ?, ?)""",
        (NPUB, ct_with_tag, NONCE, 1_700_000_000, ADDRESS),
    )

    conn.commit()
    conn.close()
    print(f"wrote {OUT_PATH}  ({os.path.getsize(OUT_PATH)} bytes)")
    print(f"  password   = {PASSWORD!r}")
    print(f"  npub       = {NPUB!r}")
    print(f"  mnemonic   = {MNEMONIC!r}")
    print(f"  ct||tag    = {ct_with_tag.hex()}")
    print(f"  nonce      = {NONCE.hex()}")


if __name__ == "__main__":
    main()
