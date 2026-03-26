//! SQLite-backed store implementing the libsignal-service-rs protocol traits.
//!
//! Stores identity keys, pre-keys, signed pre-keys, and Kyber pre-keys in a
//! single SQLite database. The database lives at `{data_dir}/signal.db`.

use libsignal_service::pre_keys::{KyberPreKeyStoreExt, PreKeysStore};
use libsignal_service::protocol::{
    Direction, GenericSignedPreKey, IdentityChange, IdentityKey, IdentityKeyPair,
    IdentityKeyStore, KyberPreKeyId, KyberPreKeyRecord, KyberPreKeyStore, PreKeyId,
    PreKeyRecord, PreKeyStore, ProtocolAddress, PublicKey, SignalProtocolError,
    SignedPreKeyId, SignedPreKeyRecord, SignedPreKeyStore,
};

use sqlx::{sqlite::SqlitePoolOptions, SqlitePool};

/// SQLite-backed Signal protocol store.
#[derive(Clone)]
pub struct SignalStore {
    pool: SqlitePool,
    identity_key_pair: IdentityKeyPair,
    registration_id: u32,
}

impl SignalStore {
    /// Open (or create) the Signal store database.
    pub async fn open(
        db_path: &str,
        identity_key_pair: IdentityKeyPair,
        registration_id: u32,
    ) -> Result<Self, sqlx::Error> {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect(&format!("sqlite:{}?mode=rwc", db_path))
            .await?;

        sqlx::query(
            "CREATE TABLE IF NOT EXISTS pre_keys (
                id INTEGER PRIMARY KEY,
                record BLOB NOT NULL
            )",
        )
        .execute(&pool)
        .await?;

        sqlx::query(
            "CREATE TABLE IF NOT EXISTS signed_pre_keys (
                id INTEGER PRIMARY KEY,
                record BLOB NOT NULL
            )",
        )
        .execute(&pool)
        .await?;

        sqlx::query(
            "CREATE TABLE IF NOT EXISTS kyber_pre_keys (
                id INTEGER PRIMARY KEY,
                record BLOB NOT NULL,
                is_last_resort INTEGER NOT NULL DEFAULT 0,
                is_stale INTEGER NOT NULL DEFAULT 0,
                stale_at TEXT
            )",
        )
        .execute(&pool)
        .await?;

        sqlx::query(
            "CREATE TABLE IF NOT EXISTS identities (
                address TEXT PRIMARY KEY,
                identity BLOB NOT NULL,
                trusted INTEGER NOT NULL DEFAULT 1
            )",
        )
        .execute(&pool)
        .await?;

        sqlx::query(
            "CREATE TABLE IF NOT EXISTS state (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL
            )",
        )
        .execute(&pool)
        .await?;

        Ok(Self {
            pool,
            identity_key_pair,
            registration_id,
        })
    }

    async fn get_state(&self, key: &str) -> Option<String> {
        sqlx::query_scalar::<_, String>("SELECT value FROM state WHERE key = ?")
            .bind(key)
            .fetch_optional(&self.pool)
            .await
            .ok()
            .flatten()
    }

    async fn set_state(&self, key: &str, value: &str) {
        let _ = sqlx::query("INSERT OR REPLACE INTO state (key, value) VALUES (?, ?)")
            .bind(key)
            .bind(value)
            .execute(&self.pool)
            .await;
    }
}

fn proto_err(msg: impl Into<String>) -> SignalProtocolError {
    SignalProtocolError::InvalidArgument(msg.into())
}

#[async_trait::async_trait(?Send)]
impl PreKeyStore for SignalStore {
    async fn get_pre_key(
        &self,
        prekey_id: PreKeyId,
    ) -> Result<PreKeyRecord, SignalProtocolError> {
        let id = u32::from(prekey_id) as i64;
        let record: Vec<u8> =
            sqlx::query_scalar("SELECT record FROM pre_keys WHERE id = ?")
                .bind(id)
                .fetch_optional(&self.pool)
                .await
                .map_err(|e| proto_err(e.to_string()))?
                .ok_or_else(|| proto_err(format!("pre_key {id} not found")))?;
        PreKeyRecord::deserialize(&record)
    }

    async fn save_pre_key(
        &mut self,
        prekey_id: PreKeyId,
        record: &PreKeyRecord,
    ) -> Result<(), SignalProtocolError> {
        let id = u32::from(prekey_id) as i64;
        let data = record.serialize()?;
        sqlx::query("INSERT OR REPLACE INTO pre_keys (id, record) VALUES (?, ?)")
            .bind(id)
            .bind(data)
            .execute(&self.pool)
            .await
            .map_err(|e| proto_err(e.to_string()))?;
        Ok(())
    }

    async fn remove_pre_key(
        &mut self,
        prekey_id: PreKeyId,
    ) -> Result<(), SignalProtocolError> {
        let id = u32::from(prekey_id) as i64;
        sqlx::query("DELETE FROM pre_keys WHERE id = ?")
            .bind(id)
            .execute(&self.pool)
            .await
            .map_err(|e| proto_err(e.to_string()))?;
        Ok(())
    }
}

#[async_trait::async_trait(?Send)]
impl SignedPreKeyStore for SignalStore {
    async fn get_signed_pre_key(
        &self,
        signed_prekey_id: SignedPreKeyId,
    ) -> Result<SignedPreKeyRecord, SignalProtocolError> {
        let id = u32::from(signed_prekey_id) as i64;
        let record: Vec<u8> =
            sqlx::query_scalar("SELECT record FROM signed_pre_keys WHERE id = ?")
                .bind(id)
                .fetch_optional(&self.pool)
                .await
                .map_err(|e| proto_err(e.to_string()))?
                .ok_or_else(|| proto_err(format!("signed_pre_key {id} not found")))?;
        SignedPreKeyRecord::deserialize(&record)
    }

    async fn save_signed_pre_key(
        &mut self,
        signed_prekey_id: SignedPreKeyId,
        record: &SignedPreKeyRecord,
    ) -> Result<(), SignalProtocolError> {
        let id = u32::from(signed_prekey_id) as i64;
        let data = record.serialize()?;
        sqlx::query("INSERT OR REPLACE INTO signed_pre_keys (id, record) VALUES (?, ?)")
            .bind(id)
            .bind(data)
            .execute(&self.pool)
            .await
            .map_err(|e| proto_err(e.to_string()))?;
        Ok(())
    }
}

#[async_trait::async_trait(?Send)]
impl KyberPreKeyStore for SignalStore {
    async fn get_kyber_pre_key(
        &self,
        kyber_prekey_id: KyberPreKeyId,
    ) -> Result<KyberPreKeyRecord, SignalProtocolError> {
        let id = u32::from(kyber_prekey_id) as i64;
        let record: Vec<u8> =
            sqlx::query_scalar("SELECT record FROM kyber_pre_keys WHERE id = ?")
                .bind(id)
                .fetch_optional(&self.pool)
                .await
                .map_err(|e| proto_err(e.to_string()))?
                .ok_or_else(|| proto_err(format!("kyber_pre_key {id} not found")))?;
        KyberPreKeyRecord::deserialize(&record)
    }

    async fn save_kyber_pre_key(
        &mut self,
        kyber_prekey_id: KyberPreKeyId,
        record: &KyberPreKeyRecord,
    ) -> Result<(), SignalProtocolError> {
        let id = u32::from(kyber_prekey_id) as i64;
        let data = record.serialize()?;
        sqlx::query("INSERT OR REPLACE INTO kyber_pre_keys (id, record) VALUES (?, ?)")
            .bind(id)
            .bind(data)
            .execute(&self.pool)
            .await
            .map_err(|e| proto_err(e.to_string()))?;
        Ok(())
    }

    async fn mark_kyber_pre_key_used(
        &mut self,
        _kyber_prekey_id: KyberPreKeyId,
        _ec_prekey_id: SignedPreKeyId,
        _base_key: &PublicKey,
    ) -> Result<(), SignalProtocolError> {
        // One-time keys are consumed on use; no-op for our store.
        Ok(())
    }
}

#[async_trait::async_trait(?Send)]
impl KyberPreKeyStoreExt for SignalStore {
    async fn store_last_resort_kyber_pre_key(
        &mut self,
        kyber_prekey_id: KyberPreKeyId,
        record: &KyberPreKeyRecord,
    ) -> Result<(), SignalProtocolError> {
        let id = u32::from(kyber_prekey_id) as i64;
        let data = record.serialize()?;
        sqlx::query(
            "INSERT OR REPLACE INTO kyber_pre_keys (id, record, is_last_resort) VALUES (?, ?, 1)",
        )
        .bind(id)
        .bind(data)
        .execute(&self.pool)
        .await
        .map_err(|e| proto_err(e.to_string()))?;
        Ok(())
    }

    async fn load_last_resort_kyber_pre_keys(
        &self,
    ) -> Result<Vec<KyberPreKeyRecord>, SignalProtocolError> {
        let rows: Vec<Vec<u8>> =
            sqlx::query_scalar("SELECT record FROM kyber_pre_keys WHERE is_last_resort = 1")
                .fetch_all(&self.pool)
                .await
                .map_err(|e| proto_err(e.to_string()))?;
        rows.iter()
            .map(|r| KyberPreKeyRecord::deserialize(r))
            .collect()
    }

    async fn remove_kyber_pre_key(
        &mut self,
        kyber_prekey_id: KyberPreKeyId,
    ) -> Result<(), SignalProtocolError> {
        let id = u32::from(kyber_prekey_id) as i64;
        sqlx::query("DELETE FROM kyber_pre_keys WHERE id = ?")
            .bind(id)
            .execute(&self.pool)
            .await
            .map_err(|e| proto_err(e.to_string()))?;
        Ok(())
    }

    async fn mark_all_one_time_kyber_pre_keys_stale_if_necessary(
        &mut self,
        stale_time: chrono::DateTime<chrono::Utc>,
    ) -> Result<(), SignalProtocolError> {
        sqlx::query(
            "UPDATE kyber_pre_keys SET is_stale = 1, stale_at = ?
             WHERE is_last_resort = 0 AND is_stale = 0",
        )
        .bind(stale_time.to_rfc3339())
        .execute(&self.pool)
        .await
        .map_err(|e| proto_err(e.to_string()))?;
        Ok(())
    }

    async fn delete_all_stale_one_time_kyber_pre_keys(
        &mut self,
        threshold: chrono::DateTime<chrono::Utc>,
        min_count: usize,
    ) -> Result<(), SignalProtocolError> {
        // Keep at least min_count stale keys.
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM kyber_pre_keys WHERE is_last_resort = 0 AND is_stale = 1",
        )
        .fetch_one(&self.pool)
        .await
        .map_err(|e| proto_err(e.to_string()))?;

        if (count as usize) <= min_count {
            return Ok(());
        }

        sqlx::query(
            "DELETE FROM kyber_pre_keys
             WHERE is_last_resort = 0 AND is_stale = 1 AND stale_at < ?
             AND id NOT IN (
                 SELECT id FROM kyber_pre_keys
                 WHERE is_last_resort = 0 AND is_stale = 1
                 ORDER BY id DESC LIMIT ?
             )",
        )
        .bind(threshold.to_rfc3339())
        .bind(min_count as i64)
        .execute(&self.pool)
        .await
        .map_err(|e| proto_err(e.to_string()))?;
        Ok(())
    }
}

#[async_trait::async_trait(?Send)]
impl IdentityKeyStore for SignalStore {
    async fn get_identity_key_pair(&self) -> Result<IdentityKeyPair, SignalProtocolError> {
        Ok(self.identity_key_pair)
    }

    async fn get_local_registration_id(&self) -> Result<u32, SignalProtocolError> {
        Ok(self.registration_id)
    }

    async fn save_identity(
        &mut self,
        address: &ProtocolAddress,
        identity: &IdentityKey,
    ) -> Result<IdentityChange, SignalProtocolError> {
        let addr = address.to_string();
        let existing: Option<Vec<u8>> =
            sqlx::query_scalar("SELECT identity FROM identities WHERE address = ?")
                .bind(&addr)
                .fetch_optional(&self.pool)
                .await
                .map_err(|e| proto_err(e.to_string()))?;

        let is_update = existing
            .as_ref()
            .map(|e| e != &identity.serialize().to_vec())
            .unwrap_or(false);

        sqlx::query("INSERT OR REPLACE INTO identities (address, identity, trusted) VALUES (?, ?, 1)")
            .bind(&addr)
            .bind(identity.serialize().to_vec())
            .execute(&self.pool)
            .await
            .map_err(|e| proto_err(e.to_string()))?;

        if is_update {
            Ok(IdentityChange::ReplacedExisting)
        } else {
            Ok(IdentityChange::NewOrUnchanged)
        }
    }

    async fn is_trusted_identity(
        &self,
        address: &ProtocolAddress,
        identity: &IdentityKey,
        _direction: Direction,
    ) -> Result<bool, SignalProtocolError> {
        let addr = address.to_string();
        let existing: Option<Vec<u8>> =
            sqlx::query_scalar("SELECT identity FROM identities WHERE address = ?")
                .bind(&addr)
                .fetch_optional(&self.pool)
                .await
                .map_err(|e| proto_err(e.to_string()))?;

        match existing {
            None => Ok(true), // First use — trust on first use.
            Some(stored) => Ok(stored == identity.serialize().to_vec()),
        }
    }

    async fn get_identity(
        &self,
        address: &ProtocolAddress,
    ) -> Result<Option<IdentityKey>, SignalProtocolError> {
        let addr = address.to_string();
        let record: Option<Vec<u8>> =
            sqlx::query_scalar("SELECT identity FROM identities WHERE address = ?")
                .bind(&addr)
                .fetch_optional(&self.pool)
                .await
                .map_err(|e| proto_err(e.to_string()))?;

        match record {
            None => Ok(None),
            Some(data) => Ok(Some(IdentityKey::decode(&data)?)),
        }
    }
}

#[async_trait::async_trait(?Send)]
impl PreKeysStore for SignalStore {
    async fn next_pre_key_id(&self) -> Result<u32, SignalProtocolError> {
        let max: Option<i64> =
            sqlx::query_scalar("SELECT MAX(id) FROM pre_keys")
                .fetch_one(&self.pool)
                .await
                .map_err(|e| proto_err(e.to_string()))?;
        Ok(max.unwrap_or(0) as u32 + 1)
    }

    async fn next_signed_pre_key_id(&self) -> Result<u32, SignalProtocolError> {
        let max: Option<i64> =
            sqlx::query_scalar("SELECT MAX(id) FROM signed_pre_keys")
                .fetch_one(&self.pool)
                .await
                .map_err(|e| proto_err(e.to_string()))?;
        Ok(max.unwrap_or(0) as u32 + 1)
    }

    async fn next_pq_pre_key_id(&self) -> Result<u32, SignalProtocolError> {
        let max: Option<i64> =
            sqlx::query_scalar("SELECT MAX(id) FROM kyber_pre_keys")
                .fetch_one(&self.pool)
                .await
                .map_err(|e| proto_err(e.to_string()))?;
        Ok(max.unwrap_or(0) as u32 + 1)
    }

    async fn signed_pre_keys_count(&self) -> Result<usize, SignalProtocolError> {
        let count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM signed_pre_keys")
                .fetch_one(&self.pool)
                .await
                .map_err(|e| proto_err(e.to_string()))?;
        Ok(count as usize)
    }

    async fn kyber_pre_keys_count(
        &self,
        last_resort: bool,
    ) -> Result<usize, SignalProtocolError> {
        let count: i64 = if last_resort {
            sqlx::query_scalar("SELECT COUNT(*) FROM kyber_pre_keys WHERE is_last_resort = 1")
                .fetch_one(&self.pool)
                .await
                .map_err(|e| proto_err(e.to_string()))?
        } else {
            sqlx::query_scalar("SELECT COUNT(*) FROM kyber_pre_keys WHERE is_last_resort = 0")
                .fetch_one(&self.pool)
                .await
                .map_err(|e| proto_err(e.to_string()))?
        };
        Ok(count as usize)
    }

    async fn signed_prekey_id(
        &self,
    ) -> Result<Option<SignedPreKeyId>, SignalProtocolError> {
        let id: Option<i64> =
            sqlx::query_scalar("SELECT MAX(id) FROM signed_pre_keys")
                .fetch_one(&self.pool)
                .await
                .map_err(|e| proto_err(e.to_string()))?;
        Ok(id.map(|i| SignedPreKeyId::from(i as u32)))
    }

    async fn last_resort_kyber_prekey_id(
        &self,
    ) -> Result<Option<KyberPreKeyId>, SignalProtocolError> {
        let id: Option<i64> = sqlx::query_scalar(
            "SELECT MAX(id) FROM kyber_pre_keys WHERE is_last_resort = 1",
        )
        .fetch_one(&self.pool)
        .await
        .map_err(|e| proto_err(e.to_string()))?;
        Ok(id.map(|i| KyberPreKeyId::from(i as u32)))
    }
}
