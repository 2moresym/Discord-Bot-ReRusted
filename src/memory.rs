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
            MAGIC_V1 => parse_legacy(lines, true)?,
            MAGIC_V2 | MAGIC_V3 => parse_legacy(lines, false)?,
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
        let content_trimmed = content.trim();

        if content_trimmed.is_empty() {
            return Ok(());
        }

        let timestamp = unix_timestamp();
        self.messages.push(MemoryMessage {
            scope: scope.clone(),
            user_id: user_id.clone(),
            role: role.clone(),
            timestamp,
            content: content_trimmed.to_owned(),
        });

        if role == "user" {
            if let Some(fact) = extract_memory_fact(content_trimmed) {
                let kind = classify_memory(&fact);
                let tags = memory_tags(&fact);
                let key = memory_key(&fact);
                self.remember_fact_internal(
                    format!("user:{user_id}"),
                    user_id,
                    fact,
                    memory_importance(content_trimmed),
                    80,
                    kind,
                    tags,
                    key,
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
        let key = memory_key(&content);

        self.remember_fact_internal(
            scope,
            user_id,
            content,
            10,
            100,
            kind,
            tags,
            key,
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
        let now = unix_timestamp();
        let query_terms = tokenize(query);
        let normalized_query = normalize_for_match(query);
        let recent = self.recent_context(scope);

        let recent_keys = recent
            .iter()
            .map(|message| message_identity(message))
            .collect::<HashSet<_>>();

        let scope_messages = self
            .messages
            .iter()
            .filter(|message| message.scope == scope)
            .collect::<Vec<_>>();

        let mut document_frequency = HashMap::<String, usize>::new();
        for message in &scope_messages {
            let unique_terms = tokenize(&message.content).into_iter().collect::<HashSet<_>>();
            for term in unique_terms {
                *document_frequency.entry(term).or_insert(0) += 1;
            }
        }

        let document_count = scope_messages.len().max(1) as f32;
        let mut scored_messages = scope_messages
            .into_iter()
            .filter(|message| !recent_keys.contains(&message_identity(message)))
            .map(|message| {
                let score = message_score(
                    message,
                    &query_terms,
                    &normalized_query,
                    &document_frequency,
                    document_count,
                    now,
                );
                (score, message)
            })
            .filter(|(score, _)| *score > 0.08)
            .collect::<Vec<_>>();

        scored_messages.sort_by(|a, b| b.0.total_cmp(&a.0));
        let mut selected = recent;
        for (_, message) in scored_messages.into_iter().take(related_limit.max(1)) {
            selected.push(message.clone());
        }
        selected.sort_by_key(|message| message.timestamp);

        let mut scored_memories = self
            .memories
            .iter()
            .enumerate()
            .filter(|(_, memory)| {
                memory.user_id == user_id
                    && (memory.scope == scope
                        || memory.scope == "global"
                        || memory.scope.starts_with("user:"))
            })
            .map(|(index, memory)| {
                let lexical = weighted_overlap_score(&query_terms, &memory.content, &document_frequency, document_count);
                let tag_hits = query_terms
                    .iter()
                    .filter(|term| memory.tags.iter().any(|tag| tag == *term))
                    .count() as f32;
                let phrase_bonus = if !normalized_query.is_empty()
                    && normalize_for_match(&memory.content).contains(&normalized_query)
                {
                    3.0
                } else {
                    0.0
                };
                let importance = f32::from(memory.importance) / 10.0;
                let confidence = f32::from(memory.confidence) / 100.0;
                let recency = recency_bonus(memory.last_accessed.max(memory.timestamp), now);
                let reinforcement = (memory.access_count.min(25) as f32) / 50.0;
                let score = lexical * 3.0
                    + tag_hits * 0.45
                    + phrase_bonus
                    + importance * 1.2
                    + confidence * 0.8
                    + recency * 0.4
                    + reinforcement;
                (score, index, memory)
            })
            .collect::<Vec<_>>();

        scored_memories.sort_by(|a, b| b.0.total_cmp(&a.0));
        let selected_memory_indices = scored_memories
            .iter()
            .take(memory_limit)
            .map(|(_, index, _)| *index)
            .collect::<Vec<_>>();

        let mut context = String::new();
        if memory_limit > 0 && !scored_memories.is_empty() {
            context.push_str("Relevant long-term memories:\n");
            for (_, _, memory) in scored_memories.into_iter().take(memory_limit) {
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
                context.push_str(if message.role == "assistant" {
                    "Vaxxer: "
                } else {
                    "User: "
                });
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
        key: String,
    ) {
        let normalized = normalize_memory(&content);
        if normalized.is_empty() {
            return;
        }

        if let Some(existing) = self.memories.iter_mut().find(|memory| {
            memory.user_id == user_id
                && ((key != "general" && memory_key(&memory.content) == key)
                    || normalize_memory(&memory.content) == normalized)
        }) {
            existing.importance = existing.importance.max(importance.min(10));
            existing.confidence = existing.confidence.max(confidence.min(100));
            existing.access_count = existing.access_count.saturating_add(1);
            existing.last_accessed = unix_timestamp();
            existing.content = content.trim().to_owned();
            existing.kind = kind;
            for tag in tags {
                if !existing.tags.iter().any(|old| old == &tag) {
                    existing.tags.push(tag);
                }
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
        let mut unique = HashMap::<(String, String), LongTermMemory>::new();
        for memory in self.memories.drain(..) {
            let key = (memory.user_id.clone(), normalize_memory(&memory.content));
            match unique.get_mut(&key) {
                Some(existing) => {
                    existing.importance = existing.importance.max(memory.importance);
                    existing.confidence = existing.confidence.max(memory.confidence);
                    existing.access_count = existing.access_count.max(memory.access_count);
                    existing.last_accessed = existing.last_accessed.max(memory.last_accessed);
                    if existing.kind == "general" && memory.kind != "general" {
                        existing.kind = memory.kind;
                    }
                    for tag in memory.tags {
                        if !existing.tags.iter().any(|old| old == &tag) {
                            existing.tags.push(tag);
                        }
                    }
                }
                None => {
                    unique.insert(key, memory);
                }
            }
        }
        self.memories = unique.into_values().collect();
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
        writeln!(file, "# Persistent history + ranked retrieval + reinforced long-term memory")?;
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
                scope.clear(); user_id.clear(); role.clear(); content.clear(); timestamp = 0;
            }
            "[memory]" => {
                kind = Some("memory");
                scope.clear(); user_id.clear(); role.clear(); content.clear(); timestamp = 0; importance = 5;
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
                scope.clear(); user_id.clear(); role.clear(); content.clear(); timestamp = 0;
            }
            "[memory]" => {
                kind = Some("memory");
                scope.clear(); user_id.clear(); role.clear(); content.clear(); timestamp = 0;
                importance = 5; confidence = 75; access_count = 0; last_accessed = 0;
                memory_kind.clear(); memory_kind.push_str("general"); tags.clear();
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
                    if last_accessed == 0 { last_accessed = timestamp; }
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
    if !(8..=400).contains(&trimmed.len()) || looks_like_question(&lower) {
        return None;
    }

    let prefixes = [
        "remember that ", "remember this ", "my name is ", "my favorite ", "my favourite ",
        "my preferred ", "i like ", "i love ", "i hate ", "i prefer ", "i use ",
        "i'm using ", "i am using ", "i code in ", "i program in ", "i live in ",
        "i'm from ", "i am from ", "i work with ", "i'm working on ", "i am working on ",
        "i'm building ", "i am building ",
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

fn looks_like_question(lower: &str) -> bool {
    lower.starts_with("why ") || lower.starts_with("what ") || lower.starts_with("who ") || lower.starts_with("how ")
        || lower.starts_with("when ") || lower.starts_with("where ") || lower.starts_with("can ") || lower.starts_with("could ")
        || lower.starts_with("would ") || lower.ends_with('?')
}

fn classify_memory(text: &str) -> String {
    let lower = text.to_ascii_lowercase();
    if lower.contains("my name is ") {
        "identity".to_owned()
    } else if lower.contains("favorite") || lower.contains("favourite") || lower.contains("prefer") || lower.contains("like") || lower.contains("love") || lower.contains("hate") {
        "preference".to_owned()
    } else if lower.contains("use ") || lower.contains("using ") || lower.contains("code in ") || lower.contains("program in ") {
        "environment".to_owned()
    } else if lower.contains("working on ") || lower.contains("building ") {
        "project".to_owned()
    } else if lower.contains("live in ") || lower.contains("i'm from ") || lower.contains("i am from ") {
        "location".to_owned()
    } else {
        "general".to_owned()
    }
}

fn memory_key(text: &str) -> String {
    let lower = text.to_ascii_lowercase();
    for marker in ["my name is ", "my favorite ", "my favourite ", "my preferred "] {
        if let Some(index) = lower.find(marker) {
            let tail = &lower[index + marker.len()..];
            if marker == "my name is " {
                return "identity:name".to_owned();
            }
            let subject = tail.split(" is ").next().unwrap_or(tail).trim();
            if !subject.is_empty() {
                return format!("preference:{subject}");
            }
        }
    }
    if lower.contains("i live in ") { return "location:home".to_owned(); }
    if lower.contains("i'm from ") || lower.contains("i am from ") { return "location:origin".to_owned(); }
    if lower.contains("i'm building ") || lower.contains("i am building ") || lower.contains("i'm working on ") || lower.contains("i am working on ") {
        return "project:current".to_owned();
    }
    "general".to_owned()
}

fn memory_tags(text: &str) -> Vec<String> {
    tokenize(text)
        .into_iter()
        .filter(|term| term.len() >= 3)
        .take(10)
        .collect()
}

fn memory_importance(text: &str) -> u8 {
    let lower = text.to_ascii_lowercase();
    let mut score = 3_u8;
    if lower.contains("my name is ") || lower.contains("remember that ") || lower.contains("remember this ") {
        score = score.max(10);
    }
    if lower.contains("my favorite ") || lower.contains("my favourite ") || lower.contains("my preferred ") || lower.contains("i live in ") || lower.contains("i'm from ") || lower.contains("i am from ") {
        score = score.max(8);
    }
    if lower.contains("i use ") || lower.contains("i'm using ") || lower.contains("i am using ") || lower.contains("i'm building ") || lower.contains("i am building ") || lower.contains("i'm working on ") || lower.contains("i am working on ") {
        score = score.max(7);
    }
    score.min(10)
}

fn tokenize(text: &str) -> Vec<String> {
    const STOPWORDS: &[&str] = &[
        "a", "an", "and", "are", "as", "at", "be", "but", "by", "for", "from", "how", "i", "in", "is", "it", "me", "my", "of", "on", "or", "so", "that", "the", "this", "to", "was", "what", "when", "where", "who", "why", "with", "you", "your", "can", "could", "would", "should",
    ];
    let stopwords = STOPWORDS.iter().copied().collect::<HashSet<_>>();
    let mut terms = Vec::new();
    let mut current = String::new();

    for character in text.chars().flat_map(char::to_lowercase) {
        if character.is_alphanumeric() || character == '_' {
            current.push(character);
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

fn message_score(
    message: &MemoryMessage,
    query_terms: &[String],
    normalized_query: &str,
    document_frequency: &HashMap<String, usize>,
    document_count: f32,
    now: u64,
) -> f32 {
    let lexical = weighted_overlap_score(query_terms, &message.content, document_frequency, document_count);
    let phrase_bonus = if !normalized_query.is_empty() && normalize_for_match(&message.content).contains(normalized_query) { 2.5 } else { 0.0 };
    let recency = recency_bonus(message.timestamp, now);
    let role_bonus = if message.role == "user" { 0.12 } else { 0.05 };
    let term_count = tokenize(&message.content).len().max(1) as f32;
    let brevity_bonus = 1.0 / (1.0 + term_count / 80.0);
    lexical * 3.2 + phrase_bonus + recency * 0.55 + role_bonus + brevity_bonus * 0.05
}

fn weighted_overlap_score(
    query_terms: &[String],
    text: &str,
    document_frequency: &HashMap<String, usize>,
    document_count: f32,
) -> f32 {
    if query_terms.is_empty() { return 0.0; }
    let text_terms = tokenize(text);
    if text_terms.is_empty() { return 0.0; }
    let term_counts = text_terms.iter().fold(HashMap::<&String, usize>::new(), |mut map, term| {
        *map.entry(term).or_insert(0) += 1;
        map
    });

    let mut matched = 0.0_f32;
    let mut possible = 0.0_f32;
    for term in query_terms {
        let df = document_frequency.get(term).copied().unwrap_or(0) as f32;
        let idf = ((document_count + 1.0) / (df + 1.0)).ln() + 1.0;
        possible += idf;
        if let Some(count) = term_counts.get(term) {
            let tf = 1.0 + (*count as f32).ln();
            matched += idf * tf;
        }
    }
    if possible == 0.0 { 0.0 } else { (matched / possible).min(2.0) }
}

fn recency_bonus(timestamp: u64, now: u64) -> f32 {
    let age_days = now.saturating_sub(timestamp) as f32 / 86_400.0;
    1.0 / (1.0 + age_days.log_1p())
}

fn message_identity(message: &MemoryMessage) -> (u64, String, String) {
    (message.timestamp, message.role.clone(), message.content.clone())
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
    if limit == 0 || text.is_empty() { return String::new(); }
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

fn decode_v2_value(value: &str) -> io::Result<String> {
    unquote(value).map_err(invalid_data)
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
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
