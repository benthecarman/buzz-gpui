//! The Buzz tools: what an agent does in Buzz, in the wire formats the
//! `buzz` CLI writes (buzz-app's `src/buzz-mcp/tools.ts`). Names, arguments
//! and descriptions are kept short because every session pays for them in
//! context. Each tool answers in plain text.

use super::{BuzzClient, BuzzEvent, Context, Template, Upload};
use nostr::nips::nip19::FromBech32;
use serde_json::{Map, Value, json};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

type Args = Map<String, Value>;
type ToolResult = Result<String, String>;

fn tool(name: &str, description: &str, properties: Value, required: &[&str]) -> Value {
    let mut schema = json!({
        "type": "object",
        "properties": properties,
        "additionalProperties": false,
    });
    if !required.is_empty() {
        schema["required"] = json!(required);
    }
    json!({ "name": name, "description": description, "inputSchema": schema })
}

/// The tools as `tools/list` describes them.
pub fn tools() -> Vec<Value> {
    let str = json!({ "type": "string" });
    let strs = json!({ "type": "array", "items": { "type": "string" } });
    let int = json!({ "type": "integer" });
    vec![
        tool(
            "send",
            "Post a message. Without channel, replies in the current thread.",
            json!({
                "text": str, "path": str, "channel": str, "reply": str,
                "files": strs, "mentions": strs,
                "final": {
                    "type": "boolean",
                    "description": "Your last message this turn; ends the turn.",
                },
            }),
            &[],
        ),
        tool(
            "edit",
            "Edit your message.",
            json!({ "id": str, "text": str }),
            &["id", "text"],
        ),
        tool(
            "react",
            "React to a message.",
            json!({ "id": str, "emoji": str, "off": { "type": "boolean" } }),
            &["id", "emoji"],
        ),
        tool(
            "read",
            "Read a thread (default: current), a channel's latest, or search with q.",
            json!({ "channel": str, "thread": str, "q": str, "limit": int, "before": int }),
            &[],
        ),
        tool("channels", "Channels you are in.", json!({ "q": str }), &[]),
        tool(
            "members",
            "A channel's members.",
            json!({ "channel": str }),
            &[],
        ),
        tool(
            "users",
            "Profiles by pubkey or name.",
            json!({ "ids": strs, "q": str }),
            &[],
        ),
        tool(
            "dm",
            "Open a DM; returns its channel.",
            json!({ "to": strs }),
            &["to"],
        ),
        tool(
            "mem_get",
            "Read memory; without slug, list slugs.",
            json!({ "slug": str }),
            &[],
        ),
        tool(
            "mem_set",
            "Write memory.",
            json!({ "slug": str, "text": str, "path": str }),
            &["slug"],
        ),
        tool(
            "canvas",
            "Read a channel's canvas, or replace it.",
            json!({ "channel": str, "text": str, "path": str }),
            &[],
        ),
    ]
}

/// Message kinds as the CLI reads them.
const MESSAGES: [u32; 5] = [9, 40002, 40008, 45001, 45003];
const THREAD: [u32; 4] = [9, 40002, 40008, 45003];
const SEARCH: [u32; 4] = [9, 40002, 45001, 45003];

fn thread_and_forum() -> Vec<u32> {
    let mut kinds = THREAD.to_vec();
    kinds.push(45001);
    kinds
}

pub(crate) fn hex(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// Runs tool `name` for an agent working in `context`. An error is a tool
/// error for the agent to correct.
pub async fn call_tool(
    client: Arc<dyn BuzzClient>,
    context: &Context,
    name: &str,
    args: &Args,
) -> ToolResult {
    let tools = Tools {
        client,
        context: context.clone(),
        members: tokio::sync::Mutex::default(),
    };
    match name {
        "send" => tools.send(args).await,
        "edit" => tools.edit(args).await,
        "react" => tools.react(args).await,
        "read" => tools.read(args).await,
        "channels" => tools.channels(args).await,
        "members" => tools.members_tool(args).await,
        "users" => tools.users(args).await,
        "dm" => tools.dm(args).await,
        "mem_get" => tools.mem_get(args).await,
        "mem_set" => tools.mem_set(args).await,
        "canvas" => tools.canvas(args).await,
        _ => Err(format!("No tool {name}")),
    }
}

/// One call's helpers, so lookups it repeats are made once.
struct Tools {
    client: Arc<dyn BuzzClient>,
    context: Context,
    members: tokio::sync::Mutex<HashMap<String, Vec<(String, String)>>>,
}

impl Tools {
    async fn query(&self, filters: Vec<Value>) -> Result<Vec<BuzzEvent>, String> {
        self.client.query(filters).await
    }

    async fn send(&self, args: &Args) -> ToolResult {
        let context = &self.context;
        let reply = text(args.get("reply")).or_else(|| {
            (!args.contains_key("channel"))
                .then(|| context.root.clone())
                .flatten()
        });
        // A reply goes to its message's channel.
        let mut channel = text(args.get("channel"));
        if channel.is_none()
            && let Some(reply) = &reply
            && Some(reply) != context.root.as_ref()
        {
            channel = tag(&self.event(reply).await?, "h");
        }
        let channel = channel
            .or_else(|| context.channel.clone())
            .ok_or("channel is required here.")?;
        let files = self.attach(&list(args.get("files"))).await?;
        let content = format!(
            "{}{}",
            self.body(args, files.links.is_empty())
                .await?
                .unwrap_or_default(),
            files.links
        );
        let thread = match &reply {
            Some(reply) => self.thread_tags(reply).await?,
            None => Vec::new(),
        };
        let mentions = self
            .mentions(&channel, &content, &list(args.get("mentions")))
            .await?;
        let mut tags = vec![vec!["h".to_string(), channel]];
        tags.extend(thread);
        tags.extend(mentions.into_iter().map(|key| vec!["p".to_string(), key]));
        tags.extend(files.tags);
        let event = self
            .client
            .publish(Template {
                kind: 9,
                content,
                tags,
            })
            .await?;
        Ok(format!("sent {}", event.id))
    }

    async fn edit(&self, args: &Args) -> ToolResult {
        let id = required(args, "id")?;
        let target = self.event(&id).await?;
        let channel = tag(&target, "h").ok_or_else(|| format!("{id} is not a channel message."))?;
        let content = required(args, "text")?;
        // An edit replaces the attachments too, so it keeps the message's.
        let mut tags = vec![vec!["h".into(), channel], vec!["e".into(), id]];
        tags.extend(
            target
                .tags
                .iter()
                .filter(|tag| tag.first().map(String::as_str) == Some("imeta"))
                .cloned(),
        );
        self.client
            .publish(Template {
                kind: 40003,
                content,
                tags,
            })
            .await?;
        Ok("edited".into())
    }

    async fn react(&self, args: &Args) -> ToolResult {
        let id = required(args, "id")?;
        let emoji = required(args, "emoji")?;
        if args.get("off") != Some(&Value::Bool(true)) {
            self.client
                .publish(Template {
                    kind: 7,
                    content: emoji,
                    tags: vec![vec!["e".into(), id]],
                })
                .await?;
            return Ok("reacted".into());
        }
        let mine = self
            .query(vec![json!({
                "kinds": [7], "#e": [id], "authors": [self.client.pubkey()], "limit": 100,
            })])
            .await?;
        let Some(reaction) = mine.iter().find(|event| event.content == emoji) else {
            return Ok("no such reaction".into());
        };
        self.client
            .publish(Template {
                kind: 5,
                content: String::new(),
                tags: vec![vec!["e".into(), reaction.id.clone()]],
            })
            .await?;
        Ok("removed".into())
    }

    async fn read(&self, args: &Args) -> ToolResult {
        let context = &self.context;
        let limit = args.get("limit").and_then(Value::as_f64);
        let before = args.get("before").and_then(Value::as_f64);
        let q = text(args.get("q"));
        let mut channel = text(args.get("channel")).or_else(|| context.channel.clone());
        let named_channel = truthy(args.get("channel"));
        let events = if let Some(q) = q {
            let mut filter =
                json!({ "kinds": SEARCH, "search": q, "limit": bound(limit, 20, 100) });
            if named_channel {
                filter["#h"] = json!([channel]);
            }
            if let Some(before) = before.filter(|before| *before != 0.0) {
                filter["until"] = json!(before);
            }
            self.query(vec![filter]).await?
        } else if let Some(id) = text(args.get("thread")).or_else(|| {
            (!args.contains_key("channel"))
                .then(|| context.root.clone())
                .flatten()
        }) {
            let mut root = id.clone();
            if Some(&id) != context.root.as_ref() {
                let event = self.event(&id).await?;
                root = root_of(&event);
                channel = tag(&event, "h").or(channel);
            }
            let channel = channel.ok_or("channel is required here.")?;
            self.query(vec![
                json!({ "kinds": THREAD, "#h": [channel], "#e": [root], "limit": bound(limit, 100, 500) }),
                json!({ "ids": [root], "kinds": thread_and_forum(), "limit": 1 }),
            ])
            .await?
        } else {
            let channel = channel.ok_or("channel is required here.")?;
            let mut filter =
                json!({ "kinds": MESSAGES, "#h": [channel], "limit": bound(limit, 30, 200) });
            if let Some(before) = before.filter(|before| *before != 0.0) {
                filter["until"] = json!(before);
            }
            self.query(vec![filter]).await?
        };
        // An edit tags only the message it changes, so a thread's are not in it.
        let ids: Vec<&str> = events
            .iter()
            .filter(|event| event.kind != 40003)
            .map(|event| event.id.as_str())
            .collect();
        let edits = if ids.is_empty() {
            Vec::new()
        } else {
            self.query(vec![json!({ "kinds": [40003], "#e": ids, "limit": 500 })])
                .await?
        };
        let mut all = events;
        all.extend(edits);
        let messages = edited(all);
        if messages.is_empty() {
            return Ok("No messages.".into());
        }
        let names = self
            .names(messages.iter().map(|event| event.pubkey.clone()).collect())
            .await?;
        Ok(messages
            .iter()
            .map(|event| {
                format!(
                    "{} {} {}: {}",
                    event.id,
                    names.get(&event.pubkey).unwrap_or(&event.pubkey),
                    minute(event.created_at),
                    event.content
                )
            })
            .collect::<Vec<_>>()
            .join("\n"))
    }

    async fn channels(&self, args: &Args) -> ToolResult {
        let memberships = self
            .query(vec![
                json!({ "kinds": [39002], "#p": [self.client.pubkey()], "limit": 500 }),
            ])
            .await?;
        let mut ids: Vec<String> = Vec::new();
        for id in memberships.iter().filter_map(|event| tag(event, "d")) {
            if !id.is_empty() && !ids.contains(&id) {
                ids.push(id);
            }
        }
        if ids.is_empty() {
            return Ok("No channels.".into());
        }
        let channels = self
            .query(vec![
                json!({ "kinds": [39000], "#d": ids, "limit": ids.len() }),
            ])
            .await?;
        let q = text(args.get("q")).map(|q| q.to_lowercase());
        let rows: Vec<String> = channels
            .iter()
            .map(|event| {
                let mut flags = Vec::new();
                if tag(event, "t").as_deref() == Some("dm") {
                    flags.push("dm");
                }
                if event
                    .tags
                    .iter()
                    .any(|t| t.first().map(String::as_str) == Some("archived"))
                {
                    flags.push("archived");
                }
                format!(
                    "{} {}{}",
                    tag(event, "d").unwrap_or_default(),
                    tag(event, "name").unwrap_or_default(),
                    if flags.is_empty() {
                        String::new()
                    } else {
                        format!(" ({})", flags.join(", "))
                    }
                )
            })
            .filter(|row| q.as_ref().is_none_or(|q| row.to_lowercase().contains(q)))
            .collect();
        Ok(or(rows.join("\n"), "No channels."))
    }

    async fn members_tool(&self, args: &Args) -> ToolResult {
        let channel = text(args.get("channel"))
            .or_else(|| self.context.channel.clone())
            .ok_or("channel is required here.")?;
        let members = self.members(&channel).await?;
        let names = self
            .names(members.iter().map(|(pubkey, _)| pubkey.clone()).collect())
            .await?;
        Ok(members
            .iter()
            .map(|(pubkey, role)| {
                format!(
                    "{pubkey} {}{}",
                    names.get(pubkey).map(String::as_str).unwrap_or(""),
                    if role == "member" {
                        String::new()
                    } else {
                        format!(" ({role})")
                    }
                )
            })
            .collect::<Vec<_>>()
            .join("\n"))
    }

    async fn users(&self, args: &Args) -> ToolResult {
        let ids = list(args.get("ids"))
            .iter()
            .map(|id| pubkey(id))
            .collect::<Result<Vec<_>, _>>()?;
        let q = text(args.get("q")).map(|q| q.to_lowercase());
        let filter = if !ids.is_empty() {
            json!({ "kinds": [0], "authors": ids, "limit": ids.len() })
        } else if let Some(q) = &q {
            json!({ "kinds": [0], "search": q, "limit": 100 })
        } else {
            return Err("Give ids or q.".into());
        };
        let profiles = self.query(vec![filter]).await?;
        let rows: Vec<String> = newest(profiles)
            .iter()
            .filter_map(|event| {
                let profile = parse(&event.content);
                let name = profile_name(&profile).unwrap_or_default();
                let about = profile
                    .get("about")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|about| !about.is_empty())
                    .map(|about| format!(" — {}", about.chars().take(120).collect::<String>()))
                    .unwrap_or_default();
                let keep = q
                    .as_ref()
                    .is_none_or(|q| !ids.is_empty() || name.to_lowercase().contains(q.as_str()));
                keep.then(|| format!("{} {name}{about}", event.pubkey))
            })
            .collect();
        Ok(or(rows.join("\n"), "No users."))
    }

    async fn dm(&self, args: &Args) -> ToolResult {
        let to = list(args.get("to"))
            .iter()
            .map(|id| pubkey(id))
            .collect::<Result<Vec<_>, _>>()?;
        if to.is_empty() {
            return Err("to is required.".into());
        }
        let me = self.client.pubkey();
        let people: HashSet<String> = std::iter::once(me.clone()).chain(to.clone()).collect();
        let find = || async {
            let events = self
                .query(vec![
                    json!({ "kinds": [39000], "#p": [me], "#t": ["dm"], "limit": 500 }),
                ])
                .await?;
            Ok::<_, String>(events.into_iter().find(|event| {
                let members: HashSet<&str> = event
                    .tags
                    .iter()
                    .filter(|t| t.first().map(String::as_str) == Some("p"))
                    .filter_map(|t| t.get(1).map(String::as_str))
                    .collect();
                members.len() == people.len()
                    && people.iter().all(|key| members.contains(key.as_str()))
            }))
        };
        let mut found = find().await?;
        if found.is_none() {
            let mut tags: Vec<Vec<String>> =
                to.iter().map(|key| vec!["p".into(), key.clone()]).collect();
            tags.push(vec!["d".into(), uuid::Uuid::new_v4().to_string()]);
            self.client
                .publish(Template {
                    kind: 41010,
                    content: String::new(),
                    tags,
                })
                .await?;
            // The relay creates the channel's metadata just after answering.
            for tries in 0..5 {
                if found.is_some() {
                    break;
                }
                if tries > 0 {
                    tokio::time::sleep(Duration::from_millis(400)).await;
                }
                found = find().await?;
            }
        }
        found
            .and_then(|event| tag(&event, "d"))
            .filter(|channel| !channel.is_empty())
            .ok_or_else(|| "The DM did not open.".into())
    }

    async fn mem_get(&self, args: &Args) -> ToolResult {
        let entries = self.client.memories().await?;
        let Some(slug) = text(args.get("slug")) else {
            let rows: Vec<String> = entries
                .iter()
                .map(|entry| format!("{} {}", entry.slug, minute(entry.created_at)))
                .collect();
            return Ok(or(rows.join("\n"), "No memory."));
        };
        let key = memory_slug(&slug)?;
        Ok(entries
            .into_iter()
            .find(|entry| entry.slug == key)
            .map(|entry| entry.body)
            .unwrap_or_else(|| format!("No memory {key}.")))
    }

    async fn mem_set(&self, args: &Args) -> ToolResult {
        let key = memory_slug(&required(args, "slug")?)?;
        let body = self.body(args, true).await?.unwrap_or_default();
        // Unreadable memory still takes a write, dated now.
        let after = self
            .client
            .memories()
            .await
            .unwrap_or_default()
            .into_iter()
            .find(|entry| entry.slug == key)
            .map_or(0, |entry| entry.created_at);
        self.client.remember(key, body, after).await?;
        Ok("saved".into())
    }

    async fn canvas(&self, args: &Args) -> ToolResult {
        let channel = text(args.get("channel"))
            .or_else(|| self.context.channel.clone())
            .ok_or("channel is required here.")?;
        let content = self.body(args, false).await?;
        let head = self
            .query(vec![
                json!({ "kinds": [40100], "#h": [channel], "limit": 1 }),
            ])
            .await?
            .into_iter()
            .next();
        let Some(content) = content else {
            return Ok(head
                .map(|head| head.content)
                .filter(|content| !content.is_empty())
                .unwrap_or_else(|| "No canvas.".into()));
        };
        // The newest canvas wins, so a write must be dated after the current one.
        let wait = head.map_or(0, |head| {
            (head.created_at as i64 + 1) * 1000 - chrono::Utc::now().timestamp_millis()
        });
        if wait > 5000 {
            return Err("The canvas was just changed; try again.".into());
        }
        if wait > 0 {
            tokio::time::sleep(Duration::from_millis(wait as u64)).await;
        }
        self.client
            .publish(Template {
                kind: 40100,
                content,
                tags: vec![vec!["h".into(), channel]],
            })
            .await?;
        Ok("saved".into())
    }

    /// `text`, or the file at `path`. `None` when neither is given and
    /// `needed` is false.
    async fn body(&self, args: &Args, needed: bool) -> Result<Option<String>, String> {
        let value = if let Some(path) = text(args.get("path")) {
            let bytes = self.client.read(path).await?;
            let text = String::from_utf8_lossy(&bytes);
            Some(text.strip_prefix('\u{feff}').unwrap_or(&text).to_string())
        } else {
            args.get("text").and_then(Value::as_str).map(str::to_string)
        };
        if value.is_none() && needed {
            return Err("Give text or path.".into());
        }
        Ok(value)
    }

    async fn event(&self, id: &str) -> Result<BuzzEvent, String> {
        if !hex(id) {
            return Err(format!("Not an event id: {id}"));
        }
        self.query(vec![
            json!({ "ids": [id], "kinds": thread_and_forum(), "limit": 1 }),
        ])
        .await?
        .into_iter()
        .next()
        .ok_or_else(|| format!("No message {id}."))
    }

    /// The e-tags of a reply to `parent`: one `reply` tag for a reply to a
    /// thread's root, `root` and `reply` for one nested deeper.
    async fn thread_tags(&self, parent: &str) -> Result<Vec<Vec<String>>, String> {
        let root = if Some(parent) == self.context.root.as_deref() {
            parent.to_string()
        } else {
            root_of(&self.event(parent).await?)
        };
        let e = |id: &str, marker: &str| vec!["e".into(), id.into(), String::new(), marker.into()];
        Ok(if root == parent {
            vec![e(&root, "reply")]
        } else {
            vec![e(&root, "root"), e(parent, "reply")]
        })
    }

    /// The channel's members and their roles.
    async fn members(&self, channel: &str) -> Result<Vec<(String, String)>, String> {
        if let Some(members) = self.members.lock().await.get(channel) {
            return Ok(members.clone());
        }
        let list = self
            .query(vec![
                json!({ "kinds": [39002], "#d": [channel], "limit": 1 }),
            ])
            .await?;
        let members: Vec<(String, String)> = list
            .first()
            .map(|list| {
                list.tags
                    .iter()
                    .filter(|t| t.first().map(String::as_str) == Some("p"))
                    .filter_map(|t| {
                        let key = t.get(1).filter(|key| !key.is_empty())?;
                        let role = t
                            .get(3)
                            .filter(|role| !role.is_empty())
                            .cloned()
                            .unwrap_or_else(|| "member".into());
                        Some((key.clone(), role))
                    })
                    .collect()
            })
            .unwrap_or_default();
        self.members
            .lock()
            .await
            .insert(channel.into(), members.clone());
        Ok(members)
    }

    /// Display names of `pubkeys` that have a profile.
    async fn names(&self, pubkeys: Vec<String>) -> Result<HashMap<String, String>, String> {
        let mut wanted: Vec<String> = Vec::new();
        for key in pubkeys {
            if !wanted.contains(&key) {
                wanted.push(key);
            }
        }
        let mut names = HashMap::new();
        if wanted.is_empty() {
            return Ok(names);
        }
        let profiles = self
            .query(vec![
                json!({ "kinds": [0], "authors": wanted, "limit": wanted.len() }),
            ])
            .await?;
        for event in newest(profiles) {
            if let Some(name) = profile_name(&parse(&event.content)) {
                names.insert(event.pubkey.clone(), name);
            }
        }
        Ok(names)
    }

    /// Who `content` notifies: `mentions`, `nostr:npub…` links, and `@Name`
    /// for a channel member, as the CLI resolves them. Each must be a member.
    async fn mentions(
        &self,
        channel: &str,
        content: &str,
        explicit: &[String],
    ) -> Result<Vec<String>, String> {
        let mut keys: Vec<String> = Vec::new();
        let add = |key: String, keys: &mut Vec<String>| {
            if !keys.contains(&key) {
                keys.push(key);
            }
        };
        for key in explicit {
            add(pubkey(key)?, &mut keys);
        }
        for npub in npub_links(content) {
            add(pubkey(&npub)?, &mut keys);
        }
        let prose = strip_code(content);
        if !prose.contains('@') && keys.is_empty() {
            return Ok(Vec::new());
        }
        let members = self.members(channel).await?;
        if prose.contains('@') {
            let mut by_name: HashMap<String, Vec<String>> = HashMap::new();
            let names = self
                .names(members.iter().map(|(key, _)| key.clone()).collect())
                .await?;
            // In member order, as buzz-app's Map iterates.
            for (key, _) in &members {
                if let Some(name) = names.get(key) {
                    by_name
                        .entry(name.to_lowercase())
                        .or_default()
                        .push(key.clone());
                }
            }
            let mut longest: Vec<&String> = by_name.keys().collect();
            longest.sort_by_key(|name| std::cmp::Reverse(name.chars().count()));
            let mut unknown = Vec::new();
            for start in at_signs(&prose) {
                let rest = prose[start..].to_lowercase();
                let name = longest.iter().find(|candidate| {
                    rest.starts_with(candidate.as_str())
                        && rest[candidate.len()..]
                            .chars()
                            .next()
                            .is_none_or(|next| next.is_whitespace() || ",;.!?:)]}".contains(next))
                });
                let found = name.and_then(|name| by_name.get(*name));
                match found {
                    Some(found) if found.len() == 1 => add(found[0].clone(), &mut keys),
                    _ => {
                        let token = name.map(|name| name.to_string()).or_else(|| {
                            let token: String = rest
                                .chars()
                                .take_while(|c| {
                                    c.is_ascii_lowercase()
                                        || c.is_ascii_digit()
                                        || matches!(c, '.' | '_' | '-')
                                })
                                .collect();
                            (!token.is_empty()).then_some(token)
                        });
                        if let Some(token) = token {
                            let ambiguous = if found.is_some_and(|found| !found.is_empty()) {
                                " (ambiguous)"
                            } else {
                                ""
                            };
                            unknown.push(format!("@{token}{ambiguous}"));
                        }
                    }
                }
            }
            if !unknown.is_empty() && explicit.is_empty() {
                return Err(format!(
                    "No single channel member is {}. Pass their pubkeys in mentions, or drop the @.",
                    unknown.join(", ")
                ));
            }
        }
        let outside: Vec<&String> = keys
            .iter()
            .filter(|key| !members.iter().any(|(member, _)| member == *key))
            .collect();
        if !outside.is_empty() {
            return Err(format!(
                "Not in this channel: {}",
                outside
                    .iter()
                    .map(|key| key.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        keys.truncate(50);
        Ok(keys)
    }

    /// Uploads each file and returns its `imeta` tags and the links to append.
    async fn attach(&self, paths: &[String]) -> Result<Attached, String> {
        let mut attached = Attached::default();
        for path in paths {
            let data = self.client.read(path.clone()).await?;
            let mime = sniff(&data)
                .ok_or_else(|| format!("{path}: only JPEG, PNG, GIF, WebP and MP4 attach."))?;
            let upload = self.client.upload(data, mime.into()).await?;
            attached.links += &format!(
                "\n![{}]({})",
                if mime.starts_with("video/") {
                    "video"
                } else {
                    "image"
                },
                upload.url
            );
            attached.tags.push(imeta(&upload));
        }
        Ok(attached)
    }
}

#[derive(Default)]
struct Attached {
    tags: Vec<Vec<String>>,
    links: String,
}

/// The root of the thread `event` is in, by its NIP-10 markers (the last
/// of each, as the relay reads them), or `event` itself.
pub(crate) fn root_of(event: &BuzzEvent) -> String {
    crate::agent_plugins::claude_code::runtime::thread_reference(event)
        .map_or_else(|| event.id.clone(), |thread| thread.root_id)
}

/// Messages oldest first, each showing its author's latest edit. Edits
/// by anyone else are left out before the newest is picked, so they can't
/// hide the author's own.
fn edited(events: Vec<BuzzEvent>) -> Vec<BuzzEvent> {
    let authors: HashMap<&str, &str> = events
        .iter()
        .filter(|event| event.kind != 40003)
        .map(|event| (event.id.as_str(), event.pubkey.as_str()))
        .collect();
    let mut edits: HashMap<String, &BuzzEvent> = HashMap::new();
    for event in events.iter().filter(|event| event.kind == 40003) {
        if let Some(target) = tag(event, "e").filter(|target| !target.is_empty()) {
            if authors.get(target.as_str()) != Some(&event.pubkey.as_str()) {
                continue;
            }
            let newer = edits
                .get(&target)
                .is_none_or(|last| last.created_at < event.created_at);
            if newer {
                edits.insert(target, event);
            }
        }
    }
    let mut messages: Vec<BuzzEvent> = events
        .iter()
        .filter(|event| event.kind != 40003)
        .map(|event| match edits.get(&event.id) {
            Some(edit) => BuzzEvent {
                content: edit.content.clone(),
                ..event.clone()
            },
            _ => event.clone(),
        })
        .collect();
    messages.sort_by_key(|event| event.created_at);
    messages
}

/// Each author's newest event, in order of first appearance.
fn newest(events: Vec<BuzzEvent>) -> Vec<BuzzEvent> {
    let mut latest: Vec<BuzzEvent> = Vec::new();
    for event in events {
        match latest.iter_mut().find(|seen| seen.pubkey == event.pubkey) {
            Some(seen) if seen.created_at < event.created_at => *seen = event,
            Some(_) => {}
            None => latest.push(event),
        }
    }
    latest
}

fn imeta(upload: &Upload) -> Vec<String> {
    let mut tag = vec![
        "imeta".to_string(),
        format!("url {}", upload.url),
        format!("m {}", upload.mime),
        format!("x {}", upload.sha256),
        format!("size {}", upload.size),
    ];
    for (key, value) in [
        ("dim", upload.dim.clone()),
        ("blurhash", upload.blurhash.clone()),
        ("thumb", upload.thumb.clone()),
        (
            "duration",
            upload.duration.map(|duration| duration.to_string()),
        ),
    ] {
        if let Some(value) = value {
            tag.push(format!("{key} {value}"));
        }
    }
    tag
}

/// An attachable type, by the file's leading bytes.
fn sniff(data: &[u8]) -> Option<&'static str> {
    let at = |offset: usize, bytes: &[u8]| data.get(offset..offset + bytes.len()) == Some(bytes);
    if at(0, &[0xff, 0xd8, 0xff]) {
        Some("image/jpeg")
    } else if at(0, &[0x89, 0x50, 0x4e, 0x47]) {
        Some("image/png")
    } else if at(0, &[0x47, 0x49, 0x46, 0x38]) {
        Some("image/gif")
    } else if at(0, &[0x52, 0x49, 0x46, 0x46]) && at(8, &[0x57, 0x45, 0x42, 0x50]) {
        Some("image/webp")
    } else if at(4, &[0x66, 0x74, 0x79, 0x70]) {
        Some("video/mp4")
    } else {
        None
    }
}

fn memory_slug(slug: &str) -> Result<String, String> {
    let key = if slug == "core" || slug.starts_with("mem/") {
        slug.to_string()
    } else {
        format!("mem/{slug}")
    };
    if !buzz_desktop::relay::agent::slug(&key) {
        return Err(format!(
            "Bad slug {slug}: use lowercase parts like notes or team/plan."
        ));
    }
    Ok(key)
}

fn pubkey(value: &str) -> Result<String, String> {
    let lower = value.to_lowercase();
    if hex(&lower) {
        return Ok(lower);
    }
    nostr::prelude::PublicKey::from_bech32(value)
        .map(|key| key.to_hex())
        .map_err(|_| format!("Not a pubkey: {value}"))
}

/// The `npub1…` of each `nostr:npub1…` link in `content`.
fn npub_links(content: &str) -> Vec<String> {
    const CHARSET: &str = "023456789acdefghjklmnpqrstuvwxyz";
    let mut links = Vec::new();
    let mut rest = content;
    while let Some(at) = rest.find("nostr:npub1") {
        let start = at + "nostr:".len();
        let body = &rest[start + 5..];
        let length = body.chars().take_while(|c| CHARSET.contains(*c)).count();
        if length >= 58 {
            links.push(rest[start..start + 5 + 58].to_string());
        }
        rest = &rest[start..];
        rest = &rest[5..];
    }
    links
}

/// `content` without fenced code blocks or inline code, which never notify.
fn strip_code(content: &str) -> String {
    // A fence opens at a line start and closes at the next line that starts
    // with one (buzz-app's /^```[\s\S]*?^```/gm).
    let starts: Vec<usize> = std::iter::once(0)
        .chain(content.match_indices('\n').map(|(index, _)| index + 1))
        .collect();
    let fence = |index: usize| content[starts[index]..].starts_with("```");
    let mut prose = String::new();
    let mut copied = 0;
    let mut index = 0;
    while index < starts.len() {
        if fence(index)
            && let Some(close) = (index + 1..starts.len()).find(|&close| fence(close))
        {
            prose += &content[copied..starts[index]];
            prose += " ";
            copied = starts[close] + 3;
            index = close + 1;
            continue;
        }
        index += 1;
    }
    prose += &content[copied..];
    // Inline code: a backtick, anything but backticks and newlines, a backtick.
    let mut out = String::new();
    let mut rest = prose.as_str();
    while let Some(open) = rest.find('`') {
        let after = &rest[open + 1..];
        match after.find(['`', '\n']) {
            Some(close) if after.as_bytes()[close] == b'`' => {
                out += &rest[..open];
                out += " ";
                rest = &after[close + 1..];
            }
            _ => {
                out += &rest[..=open];
                rest = after;
            }
        }
    }
    out += rest;
    out
}

/// Byte offsets just after each `@` that starts the text or follows
/// whitespace.
fn at_signs(prose: &str) -> Vec<usize> {
    let mut starts = Vec::new();
    let mut previous: Option<char> = None;
    for (index, c) in prose.char_indices() {
        if c == '@' && previous.is_none_or(char::is_whitespace) {
            starts.push(index + 1);
        }
        previous = Some(c);
    }
    starts
}

fn profile_name(profile: &Map<String, Value>) -> Option<String> {
    ["display_name", "name"]
        .iter()
        .filter_map(|key| profile.get(*key).and_then(Value::as_str))
        .find(|name| !name.trim().is_empty())
        .map(str::to_string)
}

fn parse(content: &str) -> Map<String, Value> {
    match serde_json::from_str(content) {
        Ok(Value::Object(map)) => map,
        _ => Map::new(),
    }
}

pub(crate) fn tag(event: &BuzzEvent, name: &str) -> Option<String> {
    event
        .tags
        .iter()
        .find(|t| t.first().map(String::as_str) == Some(name))
        .and_then(|t| t.get(1).cloned())
}

fn text(value: Option<&Value>) -> Option<String> {
    value
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_string)
}

/// JavaScript truthiness, for arguments buzz-app tests that way.
fn truthy(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null) => false,
        Some(Value::Bool(value)) => *value,
        Some(Value::Number(number)) => number.as_f64().is_some_and(|n| n != 0.0),
        Some(Value::String(text)) => !text.is_empty(),
        Some(_) => true,
    }
}

fn list(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn required(args: &Args, name: &str) -> Result<String, String> {
    args.get(name)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| format!("{name} is required."))
}

fn bound(value: Option<f64>, fallback: u32, max: u32) -> u32 {
    let value = value.unwrap_or(f64::from(fallback)).floor();
    value.clamp(1.0, f64::from(max)) as u32
}

fn or(text: String, empty: &str) -> String {
    if text.is_empty() { empty.into() } else { text }
}

/// A time to the minute, as the CLI prints it.
fn minute(seconds: u64) -> String {
    chrono::DateTime::from_timestamp(seconds as i64, 0)
        .map(|time| time.format("%Y-%m-%dT%H:%MZ").to_string())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests;
