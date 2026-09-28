#![allow(dead_code)]

use std::collections::HashMap;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum SecretStoreError {
    #[error("Secret Service error: {0}")]
    Backend(#[from] oo7::Error),
    #[error("stored secret is not valid UTF-8: {0}")]
    InvalidUtf8(#[from] std::string::FromUtf8Error),
}

pub struct SecretStore {
    keyring: oo7::Keyring,
}

impl SecretStore {
    pub async fn connect() -> Result<Self, SecretStoreError> {
        Ok(Self {
            keyring: oo7::Keyring::new().await?,
        })
    }

    pub async fn store_password(
        &self,
        account_id: &str,
        password: &str,
    ) -> Result<(), SecretStoreError> {
        let attributes = attributes(account_id);
        self.keyring
            .create_item(
                &format!("COSMIC Calendar password ({account_id})"),
                &attributes,
                oo7::Secret::text(password),
                true,
            )
            .await?;
        Ok(())
    }

    pub async fn load_password(
        &self,
        account_id: &str,
    ) -> Result<Option<String>, SecretStoreError> {
        let attrs = attributes(account_id);
        let items = self.keyring.search_items(&attrs).await?;
        if let Some(item) = items.into_iter().next() {
            return Ok(Some(String::from_utf8(
                item.secret().await?.as_bytes().to_vec(),
            )?));
        }

        // Fallback for legacy items stored under application=cosmic-calendar
        let legacy = legacy_attributes(account_id);
        let legacy_items = self.keyring.search_items(&legacy).await?;
        if let Some(item) = legacy_items.into_iter().next() {
            return Ok(Some(String::from_utf8(
                item.secret().await?.as_bytes().to_vec(),
            )?));
        }

        Ok(None)
    }

    pub async fn delete_password(&self, account_id: &str) -> Result<bool, SecretStoreError> {
        let attrs = attributes(account_id);
        let items = self.keyring.search_items(&attrs).await?;
        let mut deleted = false;
        for item in items {
            item.delete().await?;
            deleted = true;
        }

        let legacy = legacy_attributes(account_id);
        let legacy_items = self.keyring.search_items(&legacy).await?;
        for item in legacy_items {
            item.delete().await?;
            deleted = true;
        }

        Ok(deleted)
    }
}

fn attributes(account_id: &str) -> HashMap<String, String> {
    HashMap::from([
        (
            "xdg:schema".to_string(),
            "dev.edfloreshz.Accounts.Secret".to_string(),
        ),
        ("account_id".to_string(), account_id.to_string()),
        ("service".to_string(), "calendar".to_string()),
    ])
}

fn legacy_attributes(account_id: &str) -> HashMap<String, String> {
    HashMap::from([
        ("application".to_string(), "cosmic-calendar".to_string()),
        ("account-id".to_string(), account_id.to_string()),
        ("service".to_string(), "calendar".to_string()),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn password_attributes_contain_no_secret_value() {
        let attributes = attributes("account-a");
        assert_eq!(
            attributes.get("xdg:schema"),
            Some(&"dev.edfloreshz.Accounts.Secret".to_string())
        );
        assert_eq!(attributes.get("account_id"), Some(&"account-a".to_string()));
        assert_eq!(attributes.get("service"), Some(&"calendar".to_string()));
        assert!(!attributes.values().any(|value| value == "password"));
    }

    #[tokio::test]
    async fn secret_store_live_roundtrip() {
        let store = match SecretStore::connect().await {
            Ok(store) => store,
            Err(error) => {
                eprintln!("Secret Service unavailable in test environment: {error}");
                return;
            }
        };

        let account_id = "test-account-roundtrip-live";
        let test_pass = "top-secret-password-12345";

        // Clean any leftovers
        let _ = store.delete_password(account_id).await;

        // Store
        store.store_password(account_id, test_pass).await.unwrap();

        // Load and assert
        let loaded = store.load_password(account_id).await.unwrap();
        assert_eq!(loaded.as_deref(), Some(test_pass));

        // Delete
        assert!(store.delete_password(account_id).await.unwrap());

        // Verify deleted
        let after_delete = store.load_password(account_id).await.unwrap();
        assert_eq!(after_delete, None);
    }
}
