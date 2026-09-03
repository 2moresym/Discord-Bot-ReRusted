use std::{
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

const MAGIC_V1: &str = "VXM/1";
const MAGIC_V2: &str = "VXM/2";
const DEFAULT_HISTORY_LIMIT: usize = 8;

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
    pub content: String,
}

#[derive(Debug)]
pub struct MemoryStore {
    path: PathBuf,
    history_limit: usize,
    messages: Vec<MemoryMessage>,
    memories: Vec<LongTermMemory>,
}

impl MemoryStore {
    pub fn load<P: AsRef<Path>>(path: P, history_limit: usize) -> io::Result<Self> {
        let path = path.as_ref().to_path_buf();
        if !path.exists() {
            return Ok(Self {
                path,
                history_limit: history_limit.max(1),
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
            messages,
            memories,
        };
        store.compact_messages();
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

        self.messages.push(MemoryMessage {
            scope: scope.clone(),
            user_id: user_id.clone(),
            role: role.clone(),
            timestamp: unix_timestamp(),
            content: content.clone(),
        });

        if role == "user" && should_auto_remember(&content) {
            let already_saved = self.memories.iter().any(|memory| {
                memory.user_id == user_id && memory.content.eq_ignore_ascii_case(&content)
            });

            if !already_saved {
                self.memories.push(LongTermMemory {
                    scope: format!("user:{user_id}"),
                    user_id,
                    timestamp: unix_timestamp(),
                    content,
                });
            }
        }

        self.compact_messages();
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

        let already_saved = self.memories.iter().any(|memory| {
            memory.user_id == user_id && memory.content.eq_ignore_ascii_case(&content)
        });

        if !already_saved {
            self.memories.push(LongTermMemory {
                scope,
                user_id,
                timestamp: unix_timestamp(),
                content,
            });
        }

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

    pub fn memories_for(&self, scope: &str, user_id: &str) -> Vec<LongTermMemory> {
        self.memories
            .iter()
            .filter(|memory| {
                memory.scope == scope || memory.scope == "global" || memory.user_id == user_id
            })
            .cloned()
            .collect()
    }

    fn compact_messages(&mut self) {
        let mut scopes = Vec::<String>::new();
        for message in &self.messages {
            if !scopes.iter().any(|scope| scope == &message.scope) {
                scopes.push(message.scope.clone());
            }
        }

        let mut compacted = Vec::new();
        for scope in scopes {
            let mut scoped = self
                .messages
                .iter()
                .filter(|message| message.scope == scope)
                .cloned()
                .collect::<Vec<_>>();
            if scoped.len() > self.history_limit {
                let keep_from = scoped.len() - self.history_limit;
                scoped.drain(..keep_from);
            }
            compacted.extend(scoped);
        }

        compacted.sort_by_key(|message| message.timestamp);
        self.messages = compacted;
    }

    fn save(&self) -> io::Result<()> {
        let temp = self.path.with_extension("vxm.tmp");
        let mut file = fs::File::create(&temp)?;
        writeln!(file, "{MAGIC_V2}")?;
        writeln!(file, "# VxMem human-readable local memory store")?;

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
    let mut messages = Vec::new();
    let mut memories = Vec::new();
    let mut current_kind: Option<&str> = None;
    let mut scope = String::new();
    let mut user_id = String::new();
    let mut role = String::new();
    let mut timestamp = 0_u64;
    let mut content = String::new();

    for line in lines {
        match line {
            "[message]" => {
                current_kind = Some("message");
                scope.clear();
                user_id.clear();
                role.clear();
                timestamp = 0;
                content.clear();
            }
            "[memory]" => {
                current_kind = Some("memory");
                scope.clear();
                user_id.clear();
                role.clear();
                timestamp = 0;
                content.clear();
            }
            "[/message]" => {
                if current_kind == Some("message") {
                    messages.push(MemoryMessage {
                        scope: scope.clone(),
                        user_id: user_id.clone(),
                        role: role.clone(),
                        timestamp,
                        content: decode_v1(&content).map_err(invalid_data)?,
                    });
                }
                current_kind = None;
            }
            "[/memory]" => {
                if current_kind == Some("memory") {
                    memories.push(LongTermMemory {
                        scope: scope.clone(),
                        user_id: user_id.clone(),
                        timestamp,
                        content: decode_v1(&content).map_err(invalid_data)?,
                    });
                }
                current_kind = None;
            }
            line if current_kind.is_some() => {
                if let Some((key, value)) = line.split_once('=') {
                    match key {
                        "scope" => scope = decode_v1(value).map_err(invalid_data)?,
                        "user_id" => user_id = decode_v1(value).map_err(invalid_data)?,
                        "role" => role = decode_v1(value).map_err(invalid_data)?,
                        "timestamp" => timestamp = value.parse().map_err(invalid_data)?,
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

fn parse_v2<'a, I>(lines: I) -> io::Result<(Vec<MemoryMessage>, Vec<LongTermMemory>)>
where
    I: Iterator<Item = &'a str>,
{
    let mut messages = Vec::new();
    let mut memories = Vec::new();
    let mut current_kind: Option<&str> = None;
    let mut scope = String::new();
    let mut user_id = String::new();
    let mut role = String::new();
    let mut timestamp = 0_u64;
    let mut content = String::new();

    for line in lines {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        match line {
            "[message]" => {
                current_kind = Some("message");
                scope.clear();
                user_id.clear();
                role.clear();
                timestamp = 0;
                content.clear();
            }
            "[memory]" => {
                current_kind = Some("memory");
                scope.clear();
                user_id.clear();
                role.clear();
                timestamp = 0;
                content.clear();
            }
            "[/message]" => {
                if current_kind == Some("message") {
                    messages.push(MemoryMessage {
                        scope: scope.clone(),
                        user_id: user_id.clone(),
                        role: role.clone(),
                        timestamp,
                        content: content.clone(),
                    });
                }
                current_kind = None;
            }
            "[/memory]" => {
                if current_kind == Some("memory") {
                    memories.push(LongTermMemory {
                        scope: scope.clone(),
                        user_id: user_id.clone(),
                        timestamp,
                        content: content.clone(),
                    });
                }
                current_kind = None;
            }
            line if current_kind.is_some() => {
                if let Some((key, value)) = line.split_once('=') {
                    match key {
                        "scope" => scope = unquote(value).map_err(invalid_data)?,
                        "user_id" => user_id = unquote(value).map_err(invalid_data)?,
                        "role" => role = unquote(value).map_err(invalid_data)?,
                        "timestamp" => timestamp = value.parse().map_err(invalid_data)?,
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

fn should_auto_remember(text: &str) -> bool {
    let lower = text.trim().to_ascii_lowercase();
    if lower.len() < 8 || lower.len() > 300 {
        return false;
    }

    let markers = [
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
        "remember that ",
        "remember this ",
    ];

    markers.iter().any(|marker| lower.contains(marker))
}

fn unix_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
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

#[allow(dead_code)]
fn _default_history_limit() -> usize {
    DEFAULT_HISTORY_LIMIT
}
