//! Permission levels for commands (phase 4).
//!
//! bot owner > server owner > admin > mod > everyone
//!
//! - **Bot owner:** user IDs in `BOTSU_OWNERS`.
//! - **Server owner:** the owner of the server the command runs in.
//! - **Admin:** a role configured with `set adminrole`, or any role with the platform's
//!   Administrator permission (so it works without setup).
//! - **Mod:** a role configured with `set modrole`.
//!
//! Each level includes the ones below it. Working out a level costs a few API calls (server
//! owner, member roles, server roles), so the bot only does it for commands that need more than
//! "everyone".

use std::fmt;
use std::sync::Arc;

use crate::adapter::{ChatAdapter, ADMINISTRATOR};
use crate::storage::Store;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Everyone,
    Mod,
    Admin,
    ServerOwner,
    BotOwner,
}

impl fmt::Display for Level {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Level::Everyone => "anyone",
            Level::Mod => "a mod",
            Level::Admin => "an admin",
            Level::ServerOwner => "the server owner",
            Level::BotOwner => "a bot owner",
        })
    }
}

/// Stored role level names.
pub const ADMIN_ROLE: &str = "admin";
pub const MOD_ROLE: &str = "mod";

pub struct Permissions {
    store: Arc<Store>,
}

impl Permissions {
    pub fn new(store: Arc<Store>) -> Self {
        Self { store }
    }

    /// The level of `user` in `guild` (None = DM). Lookup failures count as "everyone" and are
    /// logged, so a platform hiccup never grants more than intended.
    pub async fn level(
        &self,
        chat: &dyn ChatAdapter,
        guild: Option<&str>,
        user: &str,
        is_bot_owner: bool,
    ) -> Level {
        if is_bot_owner {
            return Level::BotOwner;
        }
        let Some(guild) = guild else {
            return Level::Everyone;
        };

        match chat.guild_owner(guild).await {
            Ok(Some(owner)) if owner == user => return Level::ServerOwner,
            Ok(_) => {}
            Err(e) => tracing::warn!(error = %e, guild, "couldn't look up the server owner"),
        }

        let member_roles = match chat.member_roles(guild, user).await {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(error = %e, guild, user, "couldn't look up member roles");
                return Level::Everyone;
            }
        };
        let configured = self
            .store
            .guild_roles(chat.platform(), guild)
            .unwrap_or_else(|e| {
                tracing::warn!(error = %e, "couldn't read configured roles");
                Vec::new()
            });
        let has = |level: &str| {
            configured
                .iter()
                .any(|(l, r)| l == level && member_roles.contains(r))
        };

        if has(ADMIN_ROLE) {
            return Level::Admin;
        }
        // Administrator through any of the member's roles. On Discord the @everyone role's ID is
        // the server ID and every member has it implicitly.
        match chat.roles(guild).await {
            Ok(roles) => {
                let admin = roles.iter().any(|r| {
                    (member_roles.contains(&r.id) || r.id == guild)
                        && r.permissions & ADMINISTRATOR != 0
                });
                if admin {
                    return Level::Admin;
                }
            }
            Err(e) => tracing::warn!(error = %e, guild, "couldn't look up server roles"),
        }
        if has(MOD_ROLE) {
            return Level::Mod;
        }
        Level::Everyone
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::{Capabilities, Platform, RoleInfo};
    use crate::error::{Error, Result};
    use async_trait::async_trait;

    /// Server "g" owned by "owner". Roles: "adm" (Administrator), "modr", "plain".
    struct FakeChat {
        member_roles: Vec<String>,
        everyone_is_admin: bool,
        fail_roles: bool,
    }

    #[async_trait]
    impl ChatAdapter for FakeChat {
        fn platform(&self) -> Platform {
            Platform::Discord
        }
        fn capabilities(&self) -> Capabilities {
            Capabilities::default()
        }
        async fn send_message(&self, _: &str, _: &str) -> Result<String> {
            Ok("m".into())
        }
        async fn guild_owner(&self, _: &str) -> Result<Option<String>> {
            Ok(Some("owner".into()))
        }
        async fn member_roles(&self, _: &str, _: &str) -> Result<Vec<String>> {
            if self.fail_roles {
                return Err(Error::Internal("boom".into()));
            }
            Ok(self.member_roles.clone())
        }
        async fn roles(&self, guild: &str) -> Result<Vec<RoleInfo>> {
            let role = |id: &str, permissions| RoleInfo {
                id: id.into(),
                name: id.into(),
                permissions,
                position: 0,
            };
            let everyone = if self.everyone_is_admin {
                ADMINISTRATOR
            } else {
                0
            };
            Ok(vec![
                role(guild, everyone),
                role("adm", ADMINISTRATOR | 2048),
                role("modr", 8192),
                role("plain", 2048),
            ])
        }
    }

    fn chat(roles: &[&str]) -> FakeChat {
        FakeChat {
            member_roles: roles.iter().map(|r| r.to_string()).collect(),
            everyone_is_admin: false,
            fail_roles: false,
        }
    }

    fn perms() -> (Permissions, Arc<Store>) {
        let store = Arc::new(Store::open_in_memory().unwrap());
        (Permissions::new(store.clone()), store)
    }

    #[tokio::test]
    async fn levels() {
        let (p, store) = perms();
        let level = |c: FakeChat, user: &'static str| {
            let p = &p;
            async move { p.level(&c, Some("g"), user, false).await }
        };
        assert_eq!(level(chat(&[]), "owner").await, Level::ServerOwner);
        assert_eq!(level(chat(&["plain"]), "u").await, Level::Everyone);
        assert_eq!(
            level(chat(&["adm"]), "u").await,
            Level::Admin,
            "Administrator role"
        );
        assert_eq!(
            level(chat(&["modr"]), "u").await,
            Level::Everyone,
            "not configured yet"
        );

        store
            .add_guild_role(Platform::Discord, "g", MOD_ROLE, "modr")
            .unwrap();
        store
            .add_guild_role(Platform::Discord, "g", ADMIN_ROLE, "plain")
            .unwrap();
        assert_eq!(level(chat(&["modr"]), "u").await, Level::Mod);
        assert_eq!(
            level(chat(&["plain"]), "u").await,
            Level::Admin,
            "configured admin role"
        );
        // Configured roles only count on their own platform and server.
        assert_eq!(
            p.level(&chat(&["modr"]), Some("other"), "u", false).await,
            Level::Everyone
        );

        let mut everyone_admin = chat(&[]);
        everyone_admin.everyone_is_admin = true;
        assert_eq!(
            level(everyone_admin, "u").await,
            Level::Admin,
            "@everyone has Administrator"
        );
    }

    #[tokio::test]
    async fn bot_owner_dm_and_failures() {
        let (p, _) = perms();
        assert_eq!(
            p.level(&chat(&[]), Some("g"), "u", true).await,
            Level::BotOwner
        );
        assert_eq!(
            p.level(&chat(&["adm"]), None, "u", false).await,
            Level::Everyone,
            "DM"
        );
        let mut failing = chat(&["adm"]);
        failing.fail_roles = true;
        assert_eq!(
            p.level(&failing, Some("g"), "u", false).await,
            Level::Everyone,
            "lookup failed"
        );
    }

    #[test]
    fn levels_are_ordered() {
        assert!(Level::BotOwner > Level::ServerOwner);
        assert!(Level::ServerOwner > Level::Admin);
        assert!(Level::Admin > Level::Mod);
        assert!(Level::Mod > Level::Everyone);
    }
}
