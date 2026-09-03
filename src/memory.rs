use std::{
    collections::{HashMap, HashSet},
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

const MAGIC_V1: &str = "VXM/1";
const MAGIC_V2: &str = "VXM/2";
const MAGIC_V3: &str = "VXM/3";
const DEFAULT_HISTORY_LIMIT: usize = 50;
const DEFAULT_MAX_MESSAGES: usize = 10_000;
const DEFAULT_RELATED_MESSAGES: usize = 12;
const DEFAULT_MEMORY_LIMIT: usize = 16;
const DEFAULT_CONTEXT_CHARS: usize = 14_000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MemoryMessage {
    pub scope: String,
    pub user_id: String,
    pub role: String,
    pub timestamp: u64,
    pub content: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LongTermMemory {
    pub scope: String,
    pub user_id: String,
    pub timestamp: u64,
    pub importance: u8,
    pub content: String,
}

#[derive(Debug)]
pub struct MemoryStore {
    path: PathBuf,
    history_limit: usize,
    max_messages: usize,
    messages: Vec<MemoryMessage>,
    memories: Vec<LongTermMemory>,
}

impl MemoryStore {
    pub fn load<P: AsRef<Path>>(path: P, history_limit: usize, max_messages: usize) -> io::Result<Self> {
        let path = path.as_ref().to_path_buf();

        if !path.exists() {
            return Ok(Self {
                path,
                history_limit: history_limit.max(1),
                max_messages: max_messages.max(history_limit.max(1)),
                messages: Vec::new(),
                memories: Vec::new(),
            });
        }

        let contents = fs::read_to_string(&path)?;
        let mut lines = contents.lines();
        let version = lines.next().unwrap_or(MAGIC_V1);

        let (messages, memories) = match version {
            MAGIC_V1 => parse_v1(lines)?,
            MAGIC_V2 => parse_v2(lines)?,
            MAGIC_V3 => parse_v3(lines)?,
            other => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("unsupported VxMem header: {other}"),
                ));
            }
        };

        let mut store = Self {
            path,
            history_limit: history_limit.max(1),
            max_messages: max_messages.max(history_limit.max(1)),
            messages,
            memories,
        };
        store.enforce_message_cap();
        store.deduplicate_memories();
        Ok(store)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn remember_message(
        &mut self,
        scope: impl Into<String>,
        user_id: impl Into<String>,
        role: impl Into<String>,
        content: impl Into<String>,
    ) -> io::Result<()> {
        let scope = scope.into();
        let user_id = user_id.into();
        let role = role.into();
        let content = content.into();

        if content.trim().is_empty() {
            return Ok(());
        }

        self.messages.push(MemoryMessage {
            scope: scope.clone(),
            user_id: user_id.clone(),
            role: role.clone(),
            timestamp: unix_timestamp(),
            content: content.clone(),
        });

        if role == "user" {
            if let Some(fact) = extract_memory_fact(&content) {
                self.remember_fact_internal(
                    format!("user:{user_id}"),
                    user_id.clone(),
                    fact,
                    memory_importance(&content),
                );
            }
        }

        self.messages.sort_by_key(|message| message.timestamp);
        self.enforce_message_cap();
        self.save()
    }

    pub fn remember_fact(
        &mut self,
        scope: impl Into<String>,
        user_id: impl Into<String>,
        content: impl Into<String>,
    ) -> io::Result<()> {
        let scope = scope.into();
        let user_id = user_id.into();
        let content = content.into();
        let importance = memory_importance(&content).max(7);

        self.remember_fact_internal(scope, user_id, content, importance);
        self.save()
    }

    pub fn recent_context(&self, scope: &str) -> Vec<MemoryMessage> {
        self.messages
            .iter()
            .filter(|message| message.scope == scope)
            .rev()
            .take(self.history_limit)
            .cloned()
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect()
    }

    pub fn relevant_context(
        &self,
        scope: &str,
        user_id: &str,
        query: &str,
        related_limit: usize,
        memory_limit: usize,
        context_char_limit: usize,
    ) -> String {
        let query_terms = tokenize(query);
        let mut selected = Vec::<MemoryMessage>::new();
        let mut selected_keys = HashSet::<(u64, String, String)>::new();

        for message in self.recent_context(scope) {
            selected_keys.insert((message.timestamp, message.role.clone(), message.content.clone()));
            selected.push(message);
        }

        let mut scored_messages = self
            .messages
            .iter()
            .filter(|message| message.scope == scope)
            .filter(|message| !selected_keys.contains(&(message.timestamp, message.role.clone(), message.content.clone())))
            .map(|message| {
                let lexical = overlap_score(&query_terms, &message.content);
                let age_bonus = recency_bonus(message.timestamp);
                let role_bonus = if message.role == "user" { 0.10 } else { 0.05 };
                (lexical + age_bonus + role_bonus, message)
            })
            .filter(|(score, _)| *score > 0.0)
            .collect::<Vec<_>>();

        scored_messages.sort_by(|a, b| b.0.total_cmp(&a.0));
        for (_, message) in scored_messages.into_iter().take(related_limit) {
            selected.push(message.clone());
        }
        selected.sort_by_key(|message| message.timestamp);

        let mut scored_memories = self
            .memories
            .iter()
            .filter(|memory| {
                (memory.scope == scope || memory.scope == "global" || memory.user_id == user_id)
                    && memory.user_id == user_id
            })
            .map(|memory| {
                let lexical = overlap_score(&query_terms, &memory.content);
                let importance = f32::from(memory.importance) / 10.0;
                let recency = recency_bonus(memory.timestamp) * 0.25;
                (lexical * 3.0 + importance + recency, memory)
            })
            .collect::<Vec<_>>();

        scored_memories.sort_by(|a, b| b.0.total_cmp(&a.0));

        let mut context = String::new();

        if memory_limit > 0 && !scored_memories.is_empty() {
            context.push_str("Relevant long-term memories:\n");
            for (_, memory) in scored_memories.into_iter().take(memory_limit) {
                context.push_str("- ");
                context.push_str(&memory.content);
                context.push('\n');
            }
        }

        if !selected.is_empty() {
            if !context.is_empty() {
                context.push('\n');
            }
            context.push_str("Conversation context:\n");
            for message in selected {
                context.push_str(if message.role == "assistant" { "Vaxxer: " } else { "User: " });
                context.push_str(&message.content);
                context.push('\n');
            }
        }

        truncate_context(&context, context_char_limit.max(DEFAULT_CONTEXT_CHARS.min(context_char_limit.max(1))))
    }

    fn remember_fact_internal(&mut self, scope: String, user_id: String, content: String, importance: u8) {
        let normalized = normalize_memory(&content);
        if normalized.is_empty() {
            return;
        }

        if let Some(existing) = self.memories.iter_mut().find(|memory| {
            memory.user_id == user_id && normalize_memory(&memory.content) == normalized
        }) {
            existing.importance = existing.importance.max(importance.min(10));
            existing.timestamp = unix_timestamp();
            return;
        }

        self.memories.push(LongTermMemory {
            scope,
            user_id,
            timestamp: unix_timestamp(),
            importance: importance.min(10).max(1),
            content: content.trim().to_owned(),
        });
    }

    fn enforce_message_cap(&mut self) {
        let mut by_scope = HashMap::<String, Vec<MemoryMessage>>::new();
        for message in self.messages.drain(..) {
            by_scope.entry(message.scope.clone()).or_default().push(message);
        }

        let mut compacted = Vec::new();
        for (_, mut messages) in by_scope {
            messages.sort_by_key(|message| message.timestamp);
            if messages.len() > self.max_messages {
                let keep_from = messages.len() - self.max_messages;
                messages.drain(..keep_from);
            }
            compacted.extend(messages);
        }

        compacted.sort_by_key(|message| message.timestamp);
        self.messages = compacted;
    }

    fn deduplicate_memories(&mut self) {
        let mut seen = HashSet::<(String, String)>::new();
        self.memories.retain(|memory| {
            seen.insert((memory.user_id.clone(), normalize_memory(&memory.content)))
        });
        self.memories.sort_by_key(|memory| memory.timestamp);
    }

    fn save(&self) -> io::Result<()> {
        if let Some(parent) = self.path.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent)?;
            }
        }

        let temp = self.path.with_extension("vxm.tmp");
        let mut file = fs::File::create(&temp)?;
        writeln!(file, "{MAGIC_V3}")?;
        writeln!(file, "# VxMem intelligent local memory store")?;
        writeln!(file, "# Messages are persistent; only retrieved context is sent to the AI.")?;
        writeln!(file)?;

        for message in &self.messages {
            writeln!(file, "[message]")?;
            writeln!(file, "scope={}", quote(&message.scope))?;
            writeln!(file, "user_id={}", quote(&message.user_id))?;
            writeln!(file, "role={}", quote(&message.role))?;
            writeln!(file, "timestamp={}", message.timestamp)?;
            writeln!(file, "content={}", quote(&message.content))?;
            writeln!(file, "[/message]")?;
            writeln!(file)?;
        }

        for memory in &self.memories {
            writeln!(file, "[memory]")?;
            writeln!(file, "scope={}", quote(&memory.scope))?;
            writeln!(file, "user_id={}", quote(&memory.user_id))?;
            writeln!(file, "timestamp={}", memory.timestamp)?;
            writeln!(file, "importance={}", memory.importance)?;
            writeln!(file, "content={}", quote(&memory.content))?;
            writeln!(file, "[/memory]")?;
            writeln!(file)?;
        }

        file.flush()?;
        drop(file);
        fs::rename(temp, &self.path)
    }
}

fn parse_v1<'a, I>(lines: I) -> io::Result<(Vec<MemoryMessage>, Vec<LongTermMemory>)>
where
    I: Iterator<Item = &'a str>,
{
    parse_legacy(lines, true)
}

fn parse_v2<'a, I>(lines: I) -> io::Result<(Vec<MemoryMessage>, Vec<LongTermMemory>)>
where
    I: Iterator<Item = &'a str>,
{
    parse_legacy(lines, false)
}

fn parse_legacy<'a, I>(lines: I, encoded: bool) -> io::Result<(Vec<MemoryMessage>, Vec<LongTermMemory>)>
where
    I: Iterator<Item = &'a str>,
{
    let mut messages = Vec::new();
    let mut memories = Vec::new();
    let mut kind: Option<&str> = None;
    let mut scope = String::new();
    let mut user_id = String::new();
    let mut role = String::new();
    let mut timestamp = 0_u64;
    let mut importance = 5_u8;
    let mut content = String::new();

    for line in lines {
        match line {
            "[message]" => {
                kind = Some("message");
                scope.clear(); user_id.clear(); role.clear(); content.clear();
                timestamp = 0;
            }
            "[memory]" => {
                kind = Some("memory");
                scope.clear(); user_id.clear(); role.clear(); content.clear();
                timestamp = 0; importance = 5;
            }
            "[/message]" => {
                if kind == Some("message") {
                    messages.push(MemoryMessage {
                        scope: scope.clone(), user_id: user_id.clone(), role: role.clone(),
                        timestamp, content: if encoded { decode_v1(&content).map_err(invalid_data)? } else { decode_v2_value(&content)? },
                    });
                }
                kind = None;
            }
            "[/memory]" => {
                if kind == Some("memory") {
                    memories.push(LongTermMemory {
                        scope: scope.clone(), user_id: user_id.clone(), timestamp,
                        importance, content: if encoded { decode_v1(&content).map_err(invalid_data)? } else { decode_v2_value(&content)? },
                    });
                }
                kind = None;
            }
            line if kind.is_some() => {
                if let Some((key, value)) = line.split_once('=') {
                    match key {
                        "scope" => scope = parse_legacy_value(value, encoded)?,
                        "user_id" => user_id = parse_legacy_value(value, encoded)?,
                        "role" => role = parse_legacy_value(value, encoded)?,
                        "timestamp" => timestamp = value.parse().map_err(invalid_data)?,
                        "importance" => importance = value.parse().map_err(invalid_data)?,
                        "content" => content = value.to_owned(),
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }

    Ok((messages, memories))
}

fn parse_v3<'a, I>(lines: I) -> io::Result<(Vec<MemoryMessage>, Vec<LongTermMemory>)>
where
    I: Iterator<Item = &'a str>,
{
    let mut messages = Vec::new();
    let mut memories = Vec::new();
    let mut kind: Option<&str> = None;
    let mut scope = String::new();
    let mut user_id = String::new();
    let mut role = String::new();
    let mut timestamp = 0_u64;
    let mut importance = 5_u8;
    let mut content = String::new();

    for line in lines {
        if line.is_empty() || line.starts_with('#') { continue; }

        match line {
            "[message]" => {
                kind = Some("message");
                scope.clear(); user_id.clear(); role.clear(); content.clear(); timestamp = 0;
            }
            "[memory]" => {
                kind = Some("memory");
                scope.clear(); user_id.clear(); role.clear(); content.clear(); timestamp = 0; importance = 5;
            }
            "[/message]" => {
                if kind == Some("message") {
                    messages.push(MemoryMessage {
                        scope: scope.clone(), user_id: user_id.clone(), role: role.clone(), timestamp, content: content.clone(),
                    });
                }
                kind = None;
            }
            "[/memory]" => {
                if kind == Some("memory") {
                    memories.push(LongTermMemory {
                        scope: scope.clone(), user_id: user_id.clone(), timestamp, importance, content: content.clone(),
                    });
                }
                kind = None;
            }
            line if kind.is_some() => {
                if let Some((key, value)) = line.split_once('=') {
                    match key {
                        "scope" => scope = unquote(value).map_err(invalid_data)?,
                        "user_id" => user_id = unquote(value).map_err(invalid_data)?,
                        "role" => role = unquote(value).map_err(invalid_data)?,
                        "timestamp" => timestamp = value.parse().map_err(invalid_data)?,
                        "importance" => importance = value.parse().map_err(invalid_data)?,
                        "content" => content = unquote(value).map_err(invalid_data)?,
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }

    Ok((messages, memories))
}

fn parse_legacy_value(value: &str, encoded: bool) -> io::Result<String> {
    if encoded { decode_v1(value).map_err(invalid_data) } else { decode_v2_value(value) }
}

fn extract_memory_fact(text: &str) -> Option<String> {
    let trimmed = text.trim();
    let lower = trimmed.to_ascii_lowercase();
    if trimmed.len() < 8 || trimmed.len() > 400 { return None; }

    let prefixes = [
        "remember that ", "remember this ", "my name is ", "my favorite ", "my favourite ",
        "i like ", "i love ", "i hate ", "i prefer ", "i use ", "i'm using ", "i am using ",
        "i code in ", "i program in ", "i live in ", "i'm from ", "i am from ", "i work with ",
        "i'm working on ", "i am working on ", "i'm building ", "i am building ",
    ];

    prefixes.iter().find_map(|prefix| {
        lower.find(prefix).map(|index| {
            let original = &trimmed[index..];
            original.trim_end_matches(['.', '!', '?']).trim().to_owned()
        })
    })
}

fn memory_importance(text: &str) -> u8 {
    let lower = text.to_ascii_lowercase();
    let mut score = 3_u8;

    for marker in ["my name is ", "remember that ", "remember this ", "i live in ", "i'm from ", "i am from "] {
        if lower.contains(marker) { score = score.max(9); }
    }
    for marker in ["my favorite ", "my favourite ", "i prefer ", "i use ", "i'm using ", "i am using ", "i'm building ", "i am building "] {
        if lower.contains(marker) { score = score.max(7); }
    }
    for marker in ["i like ", "i love ", "i code in ", "i program in ", "i work with "] {
        if lower.contains(marker) { score = score.max(6); }
    }

    score.min(10)
}

fn tokenize(text: &str) -> Vec<String> {
    let mut terms = Vec::new();
    let mut current = String::new();
    for ch in text.chars().flat_map(char::to_lowercase) {
        if ch.is_alphanumeric() || ch == '_' {
            current.push(ch);
        } else if current.len() >= 2 {
            terms.push(std::mem::take(&mut current));
        } else {
            current.clear();
        }
    }
    if current.len() >= 2 { terms.push(current); }
    terms
}

fn overlap_score(query_terms: &[String], text: &str) -> f32 {
    if query_terms.is_empty() { return 0.0; }
    let terms = tokenize(text);
    if terms.is_empty() { return 0.0; }

    let term_set = terms.into_iter().collect::<HashSet<_>>();
    let mut hits = 0.0_f32;
    for term in query_terms {
        if term_set.contains(term) { hits += 1.0; }
    }

    hits / query_terms.len() as f32
}

fn recency_bonus(timestamp: u64) -> f32 {
    let now = unix_timestamp();
    let age = now.saturating_sub(timestamp) as f32;
    1.0 / (1.0 + age / 86_400.0)
}

fn normalize_memory(text: &str) -> String {
    tokenize(text).join(" ")
}

fn truncate_context(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit { return text.to_owned(); }
    let mut result = text.chars().take(limit.saturating_sub(80)).collect::<String>();
    result.push_str("\n[older context omitted]");
    result
}

fn quote(value: &str) -> String {
    let mut result = String::with_capacity(value.len() + 2);
    result.push('"');
    for character in value.chars() {
        match character {
            '\\' => result.push_str("\\\\"),
            '"' => result.push_str("\\\""),
            '\n' => result.push_str("\\n"),
            '\r' => result.push_str("\\r"),
            '\t' => result.push_str("\\t"),
            character => result.push(character),
        }
    }
    result.push('"');
    result
}

fn decode_v2_value(value: &str) -> io::Result<String> {
    unquote(value).map_err(invalid_data)
}

fn unquote(value: &str) -> Result<String, &'static str> {
    let value = value.trim();
    if value.len() < 2 || !value.starts_with('"') || !value.ends_with('"') {
        return Err("VxMem string must be enclosed in quotes");
    }
    let inner = &value[1..value.len() - 1];
    let mut result = String::with_capacity(inner.len());
    let mut escaped = false;
    for character in inner.chars() {
        if escaped {
            result.push(match character {
                '\\' => '\\',
                '"' => '"',
                'n' => '\n',
                'r' => '\r',
                't' => '\t',
                _ => return Err("invalid VxMem escape sequence"),
            });
            escaped = false;
        } else if character == '\\' {
            escaped = true;
        } else {
            result.push(character);
        }
    }
    if escaped { return Err("unterminated VxMem escape sequence"); }
    Ok(result)
}

fn decode_v1(value: &str) -> Result<String, &'static str> {
    let bytes = value.as_bytes();
    if bytes.len() % 3 != 0 { return Err("invalid VxMem escape sequence length"); }
    let mut decoded = Vec::with_capacity(bytes.len() / 3);
    for chunk in bytes.chunks_exact(3) {
        if chunk[0] != b'%' { return Err("invalid VxMem escape prefix"); }
        let high = from_hex(chunk[1]).ok_or("invalid VxMem hex digit")?;
        let low = from_hex(chunk[2]).ok_or("invalid VxMem hex digit")?;
        decoded.push((high << 4) | low);
    }
    String::from_utf8(decoded).map_err(|_| "invalid UTF-8 in VxMem record")
}

fn from_hex(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn invalid_data<E: std::fmt::Display>(error: E) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, error.to_string())
}

fn unix_timestamp() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs()
}

#[allow(dead_code)]
pub fn default_related_messages() -> usize { DEFAULT_RELATED_MESSAGES }

#[allow(dead_code)]
pub fn default_memory_limit() -> usize { DEFAULT_MEMORY_LIMIT }
