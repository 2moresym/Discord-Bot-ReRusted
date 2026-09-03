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
const MAGIC_V4: &str = "VXM/4";

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
    pub confidence: u8,
    pub access_count: u64,
    pub last_accessed: u64,
    pub kind: String,
    pub tags: Vec<String>,
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
        let history_limit = history_limit.max(1);
        let max_messages = max_messages.max(history_limit);

        if !path.exists() {
            return Ok(Self {
                path,
                history_limit,
                max_messages,
                messages: Vec::new(),
                memories: Vec::new(),
            });
        }

        let contents = fs::read_to_string(&path)?;
        let mut lines = contents.lines();
        let version = lines.next().unwrap_or(MAGIC_V1);

        let (messages, memories) = match version {
            MAGIC_V1 => parse_legacy(lines, LegacyFormat::V1)?,
            MAGIC_V2 => parse_legacy(lines, LegacyFormat::V2)?,
            MAGIC_V3 => parse_legacy(lines, LegacyFormat::V3)?,
            MAGIC_V4 => parse_v4(lines)?,
            other => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("unsupported VxMem header: {other}"),
                ));
            }
        };

        let mut store = Self {
            path,
            history_limit,
            max_messages,
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
                let importance = memory_importance(&content);
                let kind = classify_memory(&content);
                let tags = memory_tags(&content);
                self.remember_fact_internal(
                    format!("user:{user_id}"),
                    user_id,
                    fact,
                    importance,
                    85,
                    kind,
                    tags,
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
        let kind = classify_memory(&content);
        let tags = memory_tags(&content);

        self.remember_fact_internal(
            scope,
            user_id,
            content,
            memory_importance("remember this ")
                .saturating_add(5)
                .min(10),
            100,
            kind,
            tags,
        );
        self.save()
    }

    pub fn relevant_context(
        &mut self,
        scope: &str,
        user_id: &str,
        query: &str,
        related_limit: usize,
        memory_limit: usize,
        context_char_limit: usize,
    ) -> String {
        let query_terms = tokenize(query);
        let normalized_query = normalize_for_match(query);
        let now = unix_timestamp();

        let recent = self.recent_context(scope);
        let mut selected_keys = HashSet::<(u64, String, String)>::new();
        let mut selected = Vec::<MemoryMessage>::new();

        for message in recent {
            selected_keys.insert((message.timestamp, message.role.clone(), message.content.clone()));
            selected.push(message);
        }

        let mut scored_messages = self
            .messages
            .iter()
            .filter(|message| message.scope == scope)
            .filter(|message| {
                !selected_keys.contains(&(message.timestamp, message.role.clone(), message.content.clone()))
            })
            .map(|message| {
                let score = message_score(message, &query_terms, &normalized_query, now);
                (score, message)
            })
            .filter(|(score, _)| *score > 0.05)
            .collect::<Vec<_>>();

        scored_messages.sort_by(|a, b| b.0.total_cmp(&a.0));
        for (_, message) in scored_messages.into_iter().take(related_limit.max(1)) {
            selected.push(message.clone());
        }
        selected.sort_by_key(|message| message.timestamp);

        let mut scored_memories = self
            .memories
            .iter()
            .filter(|memory| {
                memory.user_id == user_id
                    && (memory.scope == scope || memory.scope == "global" || memory.scope.starts_with("user:"))
            })
            .map(|memory| {
                let lexical = overlap_score(&query_terms, &memory.content);
                let phrase_bonus = if !normalized_query.is_empty()
                    && normalize_for_match(&memory.content).contains(&normalized_query)
                {
                    2.0
                } else {
                    0.0
                };
                let importance = f32::from(memory.importance) / 10.0;
                let confidence = f32::from(memory.confidence) / 100.0;
                let recency = recency_bonus(memory.last_accessed.max(memory.timestamp), now);
                let reinforcement = (memory.access_count.min(20) as f32) / 100.0;
                (lexical * 3.0 + phrase_bonus + importance + confidence + recency + reinforcement, memory)
            })
            .collect::<Vec<_>>();

        scored_memories.sort_by(|a, b| b.0.total_cmp(&a.0));
        let selected_memory_indices = scored_memories
            .iter()
            .take(memory_limit)
            .map(|(_, memory)| self.memories.iter().position(|candidate| std::ptr::eq(candidate, *memory)))
            .flatten()
            .collect::<Vec<_>>();

        let mut context = String::new();

        if memory_limit > 0 && !scored_memories.is_empty() {
            context.push_str("Relevant long-term memories:\n");
            for (_, memory) in scored_memories.into_iter().take(memory_limit) {
                context.push_str("- [");
                context.push_str(&memory.kind);
                context.push_str("] ");
                context.push_str(&memory.content);
                context.push('\n');
            }
        }

        if !selected.is_empty() {
            if !context.is_empty() {
                context.push('\n');
            }
            context.push_str("Relevant conversation:\n");
            for message in selected {
                context.push_str(if message.role == "assistant" { "Vaxxer: " } else { "User: " });
                context.push_str(&message.content);
                context.push('\n');
            }
        }

        for index in selected_memory_indices {
            if let Some(memory) = self.memories.get_mut(index) {
                memory.access_count = memory.access_count.saturating_add(1);
                memory.last_accessed = now;
            }
        }

        truncate_context(&context, context_char_limit)
    }

    fn remember_fact_internal(
        &mut self,
        scope: String,
        user_id: String,
        content: String,
        importance: u8,
        confidence: u8,
        kind: String,
        tags: Vec<String>,
    ) {
        let normalized = normalize_memory(&content);
        if normalized.is_empty() {
            return;
        }

        if let Some(existing) = self.memories.iter_mut().find(|memory| {
            memory.user_id == user_id && normalize_memory(&memory.content) == normalized
        }) {
            existing.importance = existing.importance.max(importance.min(10));
            existing.confidence = existing.confidence.max(confidence.min(100));
            existing.access_count = existing.access_count.saturating_add(1);
            existing.last_accessed = unix_timestamp();
            for tag in tags {
                if !existing.tags.iter().any(|existing_tag| existing_tag == &tag) {
                    existing.tags.push(tag);
                }
            }
            if existing.kind == "general" && kind != "general" {
                existing.kind = kind;
            }
            return;
        }

        let now = unix_timestamp();
        self.memories.push(LongTermMemory {
            scope,
            user_id,
            timestamp: now,
            importance: importance.clamp(1, 10),
            confidence: confidence.clamp(1, 100),
            access_count: 0,
            last_accessed: now,
            kind,
            tags,
            content: content.trim().to_owned(),
        });
    }

    fn recent_context(&self, scope: &str) -> Vec<MemoryMessage> {
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
        writeln!(file, "{MAGIC_V4}")?;
        writeln!(file, "# VxMem intelligent local memory store")?;
        writeln!(file, "# Persistent history + ranked long-term memory + access tracking")?;
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
            writeln!(file, "confidence={}", memory.confidence)?;
            writeln!(file, "access_count={}", memory.access_count)?;
            writeln!(file, "last_accessed={}", memory.last_accessed)?;
            writeln!(file, "kind={}", quote(&memory.kind))?;
            writeln!(file, "tags={}", quote(&memory.tags.join(",")))?;
            writeln!(file, "content={}", quote(&memory.content))?;
            writeln!(file, "[/memory]")?;
            writeln!(file)?;
        }

        file.flush()?;
        drop(file);
        fs::rename(temp, &self.path)
    }
}

enum LegacyFormat {
    V1,
    V2,
    V3,
}

fn parse_legacy<'a, I>(lines: I, format: LegacyFormat) -> io::Result<(Vec<MemoryMessage>, Vec<LongTermMemory>)>
where
    I: Iterator<Item = &'a str>,
{
    let encoded = matches!(format, LegacyFormat::V1);
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
        if matches!(format, LegacyFormat::V2 | LegacyFormat::V3) && (line.is_empty() || line.starts_with('#')) {
            continue;
        }

        match line {
            "[message]" => {
                kind = Some("message");
                scope.clear();
                user_id.clear();
                role.clear();
                content.clear();
                timestamp = 0;
            }
            "[memory]" => {
                kind = Some("memory");
                scope.clear();
                user_id.clear();
                role.clear();
                content.clear();
                timestamp = 0;
                importance = 5;
            }
            "[/message]" => {
                if kind == Some("message") {
                    messages.push(MemoryMessage {
                        scope: parse_value(&scope, encoded)?,
                        user_id: parse_value(&user_id, encoded)?,
                        role: parse_value(&role, encoded)?,
                        timestamp,
                        content: parse_value(&content, encoded)?,
                    });
                }
                kind = None;
            }
            "[/memory]" => {
                if kind == Some("memory") {
                    let scope_value = parse_value(&scope, encoded)?;
                    let user_value = parse_value(&user_id, encoded)?;
                    let content_value = parse_value(&content, encoded)?;
                    memories.push(LongTermMemory {
                        scope: scope_value,
                        user_id: user_value,
                        timestamp,
                        importance,
                        confidence: 75,
                        access_count: 0,
                        last_accessed: timestamp,
                        kind: classify_memory(&content_value),
                        tags: memory_tags(&content_value),
                        content: content_value,
                    });
                }
                kind = None;
            }
            line if kind.is_some() => {
                if let Some((key, value)) = line.split_once('=') {
                    match key {
                        "scope" => scope = value.to_owned(),
                        "user_id" => user_id = value.to_owned(),
                        "role" => role = value.to_owned(),
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

fn parse_v4<'a, I>(lines: I) -> io::Result<(Vec<MemoryMessage>, Vec<LongTermMemory>)>
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
    let mut confidence = 75_u8;
    let mut access_count = 0_u64;
    let mut last_accessed = 0_u64;
    let mut memory_kind = String::from("general");
    let mut tags = Vec::<String>::new();
    let mut content = String::new();

    for line in lines {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        match line {
            "[message]" => {
                kind = Some("message");
                scope.clear();
                user_id.clear();
                role.clear();
                content.clear();
                timestamp = 0;
            }
            "[memory]" => {
                kind = Some("memory");
                scope.clear();
                user_id.clear();
                role.clear();
                content.clear();
                timestamp = 0;
                importance = 5;
                confidence = 75;
                access_count = 0;
                last_accessed = 0;
                memory_kind.clear();
                memory_kind.push_str("general");
                tags.clear();
            }
            "[/message]" => {
                if kind == Some("message") {
                    messages.push(MemoryMessage {
                        scope: unquote(&scope).map_err(invalid_data)?,
                        user_id: unquote(&user_id).map_err(invalid_data)?,
                        role: unquote(&role).map_err(invalid_data)?,
                        timestamp,
                        content: unquote(&content).map_err(invalid_data)?,
                    });
                }
                kind = None;
            }
            "[/memory]" => {
                if kind == Some("memory") {
                    if last_accessed == 0 {
                        last_accessed = timestamp;
                    }
                    memories.push(LongTermMemory {
                        scope: unquote(&scope).map_err(invalid_data)?,
                        user_id: unquote(&user_id).map_err(invalid_data)?,
                        timestamp,
                        importance,
                        confidence,
                        access_count,
                        last_accessed,
                        kind: memory_kind.clone(),
                        tags: tags.clone(),
                        content: unquote(&content).map_err(invalid_data)?,
                    });
                }
                kind = None;
            }
            line if kind.is_some() => {
                if let Some((key, value)) = line.split_once('=') {
                    match key {
                        "scope" => scope = value.to_owned(),
                        "user_id" => user_id = value.to_owned(),
                        "role" => role = value.to_owned(),
                        "timestamp" => timestamp = value.parse().map_err(invalid_data)?,
                        "importance" => importance = value.parse().map_err(invalid_data)?,
                        "confidence" => confidence = value.parse().map_err(invalid_data)?,
                        "access_count" => access_count = value.parse().map_err(invalid_data)?,
                        "last_accessed" => last_accessed = value.parse().map_err(invalid_data)?,
                        "kind" => memory_kind = unquote(value).map_err(invalid_data)?,
                        "tags" => {
                            let decoded = unquote(value).map_err(invalid_data)?;
                            tags = if decoded.trim().is_empty() {
                                Vec::new()
                            } else {
                                decoded.split(',').map(str::trim).filter(|tag| !tag.is_empty()).map(str::to_owned).collect()
                            };
                        }
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

fn parse_value(value: &str, encoded: bool) -> io::Result<String> {
    if encoded {
        decode_v1(value).map_err(invalid_data)
    } else {
        decode_v2_value(value)
    }
}

fn extract_memory_fact(text: &str) -> Option<String> {
    let trimmed = text.trim();
    let lower = trimmed.to_ascii_lowercase();
    if !(8..=400).contains(&trimmed.len()) {
        return None;
    }

    let prefixes = [
        "remember that ",
        "remember this ",
        "my name is ",
        "my favorite ",
        "my favourite ",
        "i like ",
        "i love ",
        "i hate ",
        "i prefer ",
        "i use ",
        "i'm using ",
        "i am using ",
        "i code in ",
        "i program in ",
        "i live in ",
        "i'm from ",
        "i am from ",
        "i work with ",
        "i'm working on ",
        "i am working on ",
        "i'm building ",
        "i am building ",
    ];

    prefixes.iter().find_map(|prefix| {
        lower.find(prefix).map(|index| {
            trimmed[index..]
                .trim_end_matches(['.', '!', '?'])
                .trim()
                .to_owned()
        })
    })
}

fn classify_memory(text: &str) -> String {
    let lower = text.to_ascii_lowercase();
    if lower.contains("my name is ") {
        "identity".to_owned()
    } else if lower.contains("my favorite ") || lower.contains("my favourite ") {
        "preference".to_owned()
    } else if lower.contains("i like ") || lower.contains("i love ") || lower.contains("i hate ") || lower.contains("i prefer ") {
        "preference".to_owned()
    } else if lower.contains("i use ") || lower.contains("i'm using ") || lower.contains("i am using ") || lower.contains("i code in ") || lower.contains("i program in ") {
        "environment".to_owned()
    } else if lower.contains("i'm working on ") || lower.contains("i am working on ") || lower.contains("i'm building ") || lower.contains("i am building ") {
        "project".to_owned()
    } else if lower.contains("i live in ") || lower.contains("i'm from ") || lower.contains("i am from ") {
        "location".to_owned()
    } else {
        "general".to_owned()
    }
}

fn memory_tags(text: &str) -> Vec<String> {
    let terms = tokenize(text);
    let mut tags = Vec::new();
    for term in terms.into_iter().filter(|term| term.len() >= 3).take(8) {
        if !tags.iter().any(|tag| tag == &term) {
            tags.push(term);
        }
    }
    tags
}

fn memory_importance(text: &str) -> u8 {
    let lower = text.to_ascii_lowercase();
    let mut score = 3_u8;

    for marker in ["my name is ", "remember that ", "remember this ", "i live in ", "i'm from ", "i am from "] {
        if lower.contains(marker) {
            score = score.max(9);
        }
    }
    for marker in ["my favorite ", "my favourite ", "i prefer ", "i use ", "i'm using ", "i am using ", "i'm building ", "i am building "] {
        if lower.contains(marker) {
            score = score.max(7);
        }
    }
    for marker in ["i like ", "i love ", "i code in ", "i program in ", "i work with "] {
        if lower.contains(marker) {
            score = score.max(6);
        }
    }

    score.min(10)
}

fn tokenize(text: &str) -> Vec<String> {
    let stopwords = [
        "a", "an", "and", "are", "as", "at", "be", "but", "by", "for", "from", "how", "i", "in", "is",
        "it", "me", "my", "of", "on", "or", "so", "that", "the", "this", "to", "was", "what", "when", "where",
        "who", "why", "with", "you", "your",
    ];
    let stopwords = stopwords.into_iter().collect::<HashSet<_>>();

    let mut terms = Vec::new();
    let mut current = String::new();
    for ch in text.chars().flat_map(char::to_lowercase) {
        if ch.is_alphanumeric() || ch == '_' {
            current.push(ch);
        } else if current.len() >= 2 {
            if !stopwords.contains(current.as_str()) {
                terms.push(std::mem::take(&mut current));
            } else {
                current.clear();
            }
        } else {
            current.clear();
        }
    }
    if current.len() >= 2 && !stopwords.contains(current.as_str()) {
        terms.push(current);
    }
    terms
}

fn overlap_score(query_terms: &[String], text: &str) -> f32 {
    if query_terms.is_empty() {
        return 0.0;
    }
    let term_set = tokenize(text).into_iter().collect::<HashSet<_>>();
    if term_set.is_empty() {
        return 0.0;
    }
    query_terms
        .iter()
        .filter(|term| term_set.contains(*term))
        .count() as f32
        / query_terms.len() as f32
}

fn message_score(message: &MemoryMessage, query_terms: &[String], normalized_query: &str, now: u64) -> f32 {
    let lexical = overlap_score(query_terms, &message.content);
    let phrase_bonus = if !normalized_query.is_empty() && normalize_for_match(&message.content).contains(normalized_query) {
        2.0
    } else {
        0.0
    };
    let recency = recency_bonus(message.timestamp, now);
    let role_bonus = if message.role == "user" { 0.10 } else { 0.05 };
    lexical * 2.5 + phrase_bonus + recency * 0.5 + role_bonus
}

fn recency_bonus(timestamp: u64, now: u64) -> f32 {
    let age = now.saturating_sub(timestamp) as f32;
    1.0 / (1.0 + age / 86_400.0)
}

fn normalize_memory(text: &str) -> String {
    tokenize(text).join(" ")
}

fn normalize_for_match(text: &str) -> String {
    text.chars()
        .flat_map(char::to_lowercase)
        .filter(|character| character.is_alphanumeric() || character.is_whitespace())
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn truncate_context(text: &str, limit: usize) -> String {
    if limit == 0 || text.is_empty() {
        return String::new();
    }
    if text.chars().count() <= limit {
        return text.to_owned();
    }
    let keep = limit.saturating_sub(80);
    let mut result = text.chars().take(keep).collect::<String>();
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

    if escaped {
        return Err("unterminated VxMem escape sequence");
    }
    Ok(result)
}

fn decode_v1(value: &str) -> Result<String, &'static str> {
    let bytes = value.as_bytes();
    if bytes.len() % 3 != 0 {
        return Err("invalid VxMem escape sequence length");
    }
    let mut decoded = Vec::with_capacity(bytes.len() / 3);
    for chunk in bytes.chunks_exact(3) {
        if chunk[0] != b'%' {
            return Err("invalid VxMem escape prefix");
        }
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
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
