//! Phase 0 spike: checks the Fluxer moderation APIs botsu uses against a real instance, through
//! botsu's own `FluxerChat` (plus a few raw calls for limits). Results go to spikes/README.md.
//!
//! Needs a test server where the bot has Manage Messages, Manage Roles, Moderate Members (and Ban
//! / Kick Members for the optional steps), a throwaway text channel, a second account to act on
//! (not an admin) and a role below the bot's highest role.
//!
//! ```text
//! ./scripts/run.ps1 -Example api_spike -- <guild> <channel> <target user> <role> [--kick] [--ban] [--old <message id>]
//! ```
//!
//! Settings: `FLUXER_TOKEN` and `FLUXER_URL` / `FLUXER_API_URL`, as for the bot (run.ps1 loads
//! them from `.env`).
//!
//! - Always: bulk delete (botsu's batches, 1 ID, 0 IDs, 101 IDs), add/remove role, timeout
//!   (2 minutes, then 30 days: over Discord's 28-day cap), the ban list, the audit log.
//! - `--old <id>`: bulk-deletes that message (pick one older than 14 days in the test channel)
//!   to see whether Fluxer has Discord's 14-day limit. It is deleted for real.
//! - `--kick`: kicks the target (they need a new invite afterwards).
//! - `--ban`: bans the target, lists it, unbans, unbans again (should fail), then a temporary ban
//!   via `ban_duration_seconds`, unbanned straight away. Runs after `--kick` if both are given.

use botsu_core::ChatAdapter;
use botsu_fluxer::{connect_rest, FluxerChat, FluxerConfig, FluxerEndpoint};
use fluxer::http::Http;
use serde_json::{json, Value};
use time::OffsetDateTime;

/// Contains a non-ASCII character and a `%` to check the header's percent-encoding round-trip.
const REASON: &str = "botsu API spike: café 100% ✓";

struct Args {
    guild: String,
    channel: String,
    target: String,
    role: String,
    kick: bool,
    ban: bool,
    old: Option<String>,
}

fn parse_args() -> Result<Args, String> {
    let mut positional = Vec::new();
    let (mut kick, mut ban, mut old) = (false, false, None);
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--kick" => kick = true,
            "--ban" => ban = true,
            "--old" => old = Some(it.next().ok_or("--old needs a message ID")?),
            _ => positional.push(a),
        }
    }
    let [guild, channel, target, role]: [String; 4] = positional.try_into().map_err(|_| {
        "usage: api_spike <guild> <channel> <target user> <role> [--kick] [--ban] [--old <id>]"
            .to_owned()
    })?;
    Ok(Args {
        guild,
        channel,
        target,
        role,
        kick,
        ban,
        old,
    })
}

/// Collects one line per check and prints a summary at the end.
#[derive(Default)]
struct Report(Vec<(bool, String)>);

impl Report {
    fn check(&mut self, ok: bool, what: impl Into<String>) {
        let what = what.into();
        println!("{} {what}", if ok { "PASS" } else { "FAIL" });
        self.0.push((ok, what));
    }
    fn info(&self, what: impl AsRef<str>) {
        println!("     {}", what.as_ref());
    }
}

/// A raw REST call: `(status, body as JSON or Null)`.
async fn raw(
    http: &Http,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> Result<(u16, Value), String> {
    let url = format!("{}{path}", http.base_url);
    let c = &http.client;
    let mut req = match method {
        "GET" => c.get(&url),
        "POST" => c.post(&url),
        "PUT" => c.put(&url),
        "PATCH" => c.patch(&url),
        "DELETE" => c.delete(&url),
        _ => return Err(format!("unknown method {method}")),
    };
    req = match body {
        Some(b) => req.json(&b),
        None => req,
    };
    let resp = req.send().await.map_err(|e| e.to_string())?;
    let status = resp.status().as_u16();
    let text = resp.text().await.unwrap_or_default();
    Ok((status, serde_json::from_str(&text).unwrap_or(Value::Null)))
}

fn rfc3339(unix: i64) -> String {
    OffsetDateTime::from_unix_timestamp(unix)
        .unwrap()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap()
}

fn now() -> i64 {
    OffsetDateTime::now_utc().unix_timestamp()
}

fn endpoint() -> FluxerEndpoint {
    let var = |k| {
        std::env::var(k)
            .ok()
            .filter(|v: &String| !v.trim().is_empty())
    };
    match (var("FLUXER_API_URL"), var("FLUXER_URL")) {
        (Some(api), _) => FluxerEndpoint::ApiUrl(api),
        (None, Some(url)) => FluxerEndpoint::Instance(url),
        (None, None) => FluxerEndpoint::Default,
    }
}

#[tokio::main]
async fn main() {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(2);
        }
    };
    let token = std::env::var("FLUXER_TOKEN").unwrap_or_default();
    if token.trim().is_empty() {
        eprintln!("FLUXER_TOKEN is not set");
        std::process::exit(2);
    }
    let config = FluxerConfig {
        token,
        endpoint: endpoint(),
    };
    let (http, bot_id, _) = match connect_rest(&config).await {
        Ok(c) => c,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    };
    println!("API: {}  bot: {bot_id}\n", http.base_url);
    let chat = FluxerChat::new(http.clone(), bot_id.clone());
    let mut r = Report::default();

    hierarchy(&chat, &args, &bot_id, &r).await;
    bulk_delete(&chat, &http, &args, &mut r).await;
    roles(&chat, &args, &mut r).await;
    timeouts(&chat, &http, &args, &mut r).await;
    match chat.bans(&args.guild).await {
        Ok(b) => r.check(true, format!("list bans ({} entries)", b.len())),
        Err(e) => r.check(false, format!("list bans: {e}")),
    }
    if args.kick {
        kick(&chat, &args, &mut r).await;
    }
    if args.ban {
        bans(&chat, &http, &args, &mut r).await;
    }
    audit_log(&http, &args, &bot_id, &mut r).await;

    let failed = r.0.iter().filter(|(ok, _)| !ok).count();
    println!("\n{} checks, {failed} failed", r.0.len());
    for (_, what) in r.0.iter().filter(|(ok, _)| !ok) {
        println!("  FAIL {what}");
    }
}

/// Prints the role positions involved, so a hierarchy failure later is easy to explain.
async fn hierarchy(chat: &FluxerChat, a: &Args, bot_id: &str, r: &Report) {
    let roles = match chat.roles(&a.guild).await {
        Ok(roles) => roles,
        Err(e) => return r.info(format!("couldn't read roles: {e}")),
    };
    let pos = |id: &str| roles.iter().find(|x| x.id == id).map(|x| x.position);
    let top = |user: &str| {
        let user = user.to_owned();
        async move {
            chat.member_roles(&a.guild, &user)
                .await
                .ok()
                .and_then(|ids| ids.iter().filter_map(|id| pos(id)).max())
        }
    };
    r.info(format!(
        "{} roles; test role position {:?}; bot's highest {:?}; target's highest {:?}",
        roles.len(),
        pos(&a.role),
        top(bot_id).await,
        top(&a.target).await
    ));
}

async fn bulk_delete(chat: &FluxerChat, http: &Http, a: &Args, r: &mut Report) {
    let mut ids = Vec::new();
    for i in 1..=3 {
        match chat
            .send_message(
                &a.channel,
                &format!("botsu API spike: bulk delete test {i}/3"),
            )
            .await
        {
            Ok(id) => ids.push(id),
            Err(e) => return r.check(false, format!("send test messages: {e}")),
        }
    }
    let res = chat.bulk_delete(&a.channel, &ids[..2]).await;
    r.check(
        res.is_ok(),
        format!("bulk delete 2 fresh messages (botsu's call) {res:?}"),
    );

    let path = format!("/channels/{}/messages/bulk-delete", a.channel);
    let res = raw(
        http,
        "POST",
        &path,
        Some(json!({ "message_ids": [ids[2]] })),
    )
    .await;
    r.check(
        matches!(res, Ok((204, _))),
        format!("bulk delete with 1 ID (Discord needs 2+): {res:?}"),
    );
    let res = raw(http, "POST", &path, Some(json!({ "message_ids": [] }))).await;
    r.check(
        matches!(res, Ok((400, _))),
        format!("bulk delete with 0 IDs is refused: {res:?}"),
    );
    // Made-up IDs: the docs say unknown IDs are skipped, so nothing real is deleted.
    let fake: Vec<String> = (0..101).map(|i| (1_000_000 + i).to_string()).collect();
    let res = raw(http, "POST", &path, Some(json!({ "message_ids": fake }))).await;
    r.check(
        matches!(res, Ok((400, _))),
        format!("bulk delete with 101 IDs is refused: {res:?}"),
    );
    let fake: Vec<String> = (0..100).map(|i| (1_000_000 + i).to_string()).collect();
    let res = raw(http, "POST", &path, Some(json!({ "message_ids": fake }))).await;
    r.check(
        matches!(res, Ok((204, _))),
        format!("bulk delete with 100 unknown IDs is accepted: {res:?}"),
    );

    if let Some(old) = &a.old {
        let res = raw(http, "POST", &path, Some(json!({ "message_ids": [old] }))).await;
        r.info(format!(
            "bulk delete of message {old} (should be > 14 days old): {res:?}"
        ));
        let gone = raw(
            http,
            "GET",
            &format!("/channels/{}/messages/{old}", a.channel),
            None,
        )
        .await;
        r.check(
            matches!(gone, Ok((404, _))),
            format!("old message is gone afterwards (no 14-day limit): {gone:?}"),
        );
    }
}

async fn roles(chat: &FluxerChat, a: &Args, r: &mut Report) {
    let has = || async {
        chat.member_roles(&a.guild, &a.target)
            .await
            .map(|ids| ids.contains(&a.role))
    };
    let res = chat.add_role(&a.guild, &a.target, &a.role).await;
    r.check(res.is_ok(), format!("add role {res:?}"));
    let now_has = has().await;
    r.check(
        matches!(now_has, Ok(true)),
        format!("member has the role: {now_has:?}"),
    );
    let res = chat.remove_role(&a.guild, &a.target, &a.role).await;
    r.check(res.is_ok(), format!("remove role {res:?}"));
    let now_has = has().await;
    r.check(
        matches!(now_has, Ok(false)),
        format!("member no longer has the role: {now_has:?}"),
    );
    let res = chat.remove_role(&a.guild, &a.target, &a.role).await;
    r.check(
        res.is_ok(),
        format!("removing a role they don't have still succeeds: {res:?}"),
    );
}

async fn timeouts(chat: &FluxerChat, http: &Http, a: &Args, r: &mut Report) {
    let member = format!("/guilds/{}/members/{}", a.guild, a.target);
    let until = |v: &Value| {
        v["communication_disabled_until"]
            .as_str()
            .map(str::to_owned)
    };

    let res = chat
        .timeout(&a.guild, &a.target, Some(now() + 120), Some(REASON))
        .await;
    r.check(
        res.is_ok(),
        format!("timeout 2 minutes (botsu's call) {res:?}"),
    );
    let got = raw(http, "GET", &member, None).await;
    let set = got.as_ref().ok().and_then(|(_, v)| until(v));
    r.check(
        set.is_some(),
        format!("member shows communication_disabled_until: {set:?}"),
    );

    let res = chat.timeout(&a.guild, &a.target, None, Some(REASON)).await;
    r.check(res.is_ok(), format!("clear timeout (botsu's call) {res:?}"));
    let got = raw(http, "GET", &member, None).await;
    let set = got.as_ref().ok().and_then(|(_, v)| until(v));
    r.check(set.is_none(), format!("timeout cleared: {set:?}"));

    let res = raw(
        http,
        "PATCH",
        &member,
        Some(json!({ "communication_disabled_until": rfc3339(now() + 30 * 86_400) })),
    )
    .await;
    r.check(
        matches!(res, Ok((200, _))),
        format!(
            "30-day timeout accepted (Discord caps at 28): {}",
            res.as_ref()
                .map_or_else(|e| e.clone(), |(s, _)| s.to_string())
        ),
    );
    let res = chat.timeout(&a.guild, &a.target, None, None).await;
    r.check(res.is_ok(), format!("clear the 30-day timeout {res:?}"));
}

async fn kick(chat: &FluxerChat, a: &Args, r: &mut Report) {
    let res = chat.kick(&a.guild, &a.target, Some(REASON)).await;
    r.check(res.is_ok(), format!("kick (botsu's call) {res:?}"));
    let member = chat.member_roles(&a.guild, &a.target).await;
    r.check(
        member.is_err(),
        format!("target is no longer a member: {member:?}"),
    );
}

async fn bans(chat: &FluxerChat, http: &Http, a: &Args, r: &mut Report) {
    let res = chat.ban(&a.guild, &a.target, Some(REASON), 0).await;
    r.check(res.is_ok(), format!("ban (botsu's call) {res:?}"));
    let listed = chat.bans(&a.guild).await.map(|bans| {
        bans.into_iter()
            .find(|b| b.user_id == a.target)
            .map(|b| b.reason)
    });
    r.check(
        matches!(&listed, Ok(Some(Some(reason))) if reason == REASON),
        format!("ban list has the target with the reason intact: {listed:?}"),
    );
    let res = chat.unban(&a.guild, &a.target, Some(REASON)).await;
    r.check(res.is_ok(), format!("unban (botsu's call) {res:?}"));
    let res = chat.unban(&a.guild, &a.target, None).await;
    r.check(
        res.is_err(),
        format!("unbanning someone not banned fails: {res:?}"),
    );

    let ban = format!("/guilds/{}/bans/{}", a.guild, a.target);
    let res = raw(
        http,
        "PUT",
        &ban,
        Some(json!({ "ban_duration_seconds": 3600, "reason": REASON })),
    )
    .await;
    r.check(
        matches!(res, Ok((204, _))),
        format!("temporary ban (ban_duration_seconds) {res:?}"),
    );
    let got = raw(http, "GET", &ban, None).await;
    let expires = got
        .as_ref()
        .ok()
        .and_then(|(_, v)| v["expires_at"].as_str().map(str::to_owned));
    r.check(
        expires.is_some(),
        format!("ban shows expires_at: {expires:?}"),
    );
    let res = chat.unban(&a.guild, &a.target, None).await;
    r.check(res.is_ok(), format!("lift the temporary ban {res:?}"));
}

/// The spike's entries in the audit log, to see whether the reason header arrived intact.
async fn audit_log(http: &Http, a: &Args, bot_id: &str, r: &mut Report) {
    let res = raw(
        http,
        "GET",
        &format!("/guilds/{}/audit-logs?limit=25", a.guild),
        None,
    )
    .await;
    let entries = match res {
        Ok((200, v)) => v["audit_log_entries"]
            .as_array()
            .cloned()
            .unwrap_or_default(),
        other => return r.check(false, format!("read audit log: {other:?}")),
    };
    let mine: Vec<&Value> = entries
        .iter()
        .filter(|e| e["user_id"].as_str() == Some(bot_id))
        .collect();
    for e in &mine {
        r.info(format!(
            "audit: action {} reason {:?}",
            e["action_type"], e["reason"]
        ));
    }
    let intact = mine.iter().any(|e| e["reason"].as_str() == Some(REASON));
    r.check(
        intact,
        format!(
            "audit log has the reason with non-ASCII and % intact ({} of the bot's entries)",
            mine.len()
        ),
    );
}
