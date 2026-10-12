//! 容错文本 VDF（Valve KeyValues）解析器。
//!
//! 支持：带引号/无引号 token、嵌套对象、`//` 注释、条件标签 `[...]`（忽略）。
//! 转义处理保守：只识别 `\"` 与 `\\`，其余反斜杠按字面保留（防止路径被误吞）。
//! 解析失败返回 Err，由调用方降级跳过，绝不 panic。

#[derive(Debug, Clone)]
pub enum Value {
    Str(String),
    Obj(Node),
}

#[derive(Debug, Clone, Default)]
pub struct Node {
    pub entries: Vec<(String, Value)>,
}

impl Node {
    /// VDF 键大小写不敏感。
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.entries
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(key))
            .map(|(_, v)| v)
    }

    pub fn get_str(&self, key: &str) -> Option<&str> {
        match self.get(key) {
            Some(Value::Str(s)) => Some(s.as_str()),
            _ => None,
        }
    }

    pub fn get_obj(&self, key: &str) -> Option<&Node> {
        match self.get(key) {
            Some(Value::Obj(n)) => Some(n),
            _ => None,
        }
    }

    pub fn get_u64(&self, key: &str) -> Option<u64> {
        self.get_str(key).and_then(|s| s.trim().parse::<u64>().ok())
    }
}

pub fn parse(text: &str) -> Result<Node, String> {
    let mut p = Parser {
        b: text.as_bytes(),
        pos: 0,
    };
    let mut root = Node::default();
    loop {
        p.skip_trivia();
        if p.at_end() {
            break;
        }
        let key = p.token()?;
        p.skip_trivia();
        if p.at_end() {
            return Err(format!("key '{}' 后缺少值", key));
        }
        let val = p.value()?;
        root.entries.push((key, val));
    }
    Ok(root)
}

struct Parser<'a> {
    b: &'a [u8],
    pos: usize,
}

impl<'a> Parser<'a> {
    fn at_end(&self) -> bool {
        self.pos >= self.b.len()
    }

    fn peek(&self) -> u8 {
        *self.b.get(self.pos).unwrap_or(&0)
    }

    fn skip_trivia(&mut self) {
        loop {
            while !self.at_end() && self.peek().is_ascii_whitespace() {
                self.pos += 1;
            }
            if self.at_end() {
                return;
            }
            // // 行注释
            if self.peek() == b'/' && self.b.get(self.pos + 1) == Some(&b'/') {
                while !self.at_end() && self.peek() != b'\n' {
                    self.pos += 1;
                }
                continue;
            }
            // [platform] 条件标签
            if self.peek() == b'[' {
                while !self.at_end() && self.peek() != b']' {
                    self.pos += 1;
                }
                if !self.at_end() {
                    self.pos += 1; // 吃掉 ']'
                }
                continue;
            }
            return;
        }
    }

    /// 读取一个 token：带引号字符串或无引号裸词。
    fn token(&mut self) -> Result<String, String> {
        if self.at_end() {
            return Err("意外的文件结尾".to_string());
        }
        match self.peek() {
            b'"' => self.quoted(),
            b'{' | b'}' => Err(format!("位置 {} 处出现意外的 '{}'", self.pos, self.peek() as char)),
            _ => {
                let start = self.pos;
                while !self.at_end()
                    && !self.peek().is_ascii_whitespace()
                    && self.peek() != b'{'
                    && self.peek() != b'}'
                {
                    self.pos += 1;
                }
                if self.pos == start {
                    return Err(format!("位置 {} 处为空 token", self.pos));
                }
                Ok(String::from_utf8_lossy(&self.b[start..self.pos]).into_owned())
            }
        }
    }

    fn quoted(&mut self) -> Result<String, String> {
        self.pos += 1; // 吃掉开引号
        let mut out: Vec<u8> = Vec::new();
        loop {
            if self.at_end() {
                return Err("字符串未闭合".to_string());
            }
            let c = self.peek();
            match c {
                b'"' => {
                    self.pos += 1;
                    break;
                }
                b'\\' => {
                    let next = self.b.get(self.pos + 1).copied();
                    match next {
                        Some(b'"') => {
                            out.push(b'"');
                            self.pos += 2;
                        }
                        Some(b'\\') => {
                            out.push(b'\\');
                            self.pos += 2;
                        }
                        _ => {
                            // 未知转义：保守保留反斜杠本身
                            out.push(b'\\');
                            self.pos += 1;
                        }
                    }
                }
                _ => {
                    out.push(c);
                    self.pos += 1;
                }
            }
        }
        Ok(String::from_utf8_lossy(&out).into_owned())
    }

    fn value(&mut self) -> Result<Value, String> {
        if self.peek() == b'{' {
            self.pos += 1;
            Ok(Value::Obj(self.obj_body()?))
        } else {
            Ok(Value::Str(self.token()?))
        }
    }

    fn obj_body(&mut self) -> Result<Node, String> {
        let mut node = Node::default();
        loop {
            self.skip_trivia();
            if self.at_end() {
                return Err("对象未闭合（缺少 '}'）".to_string());
            }
            if self.peek() == b'}' {
                self.pos += 1;
                return Ok(node);
            }
            let key = self.token()?;
            self.skip_trivia();
            if self.at_end() {
                return Err(format!("key '{}' 后缺少值", key));
            }
            let val = self.value()?;
            node.entries.push((key, val));
        }
    }
}

/// 从 remotecache.vdf 的解析树中提取文件条目列表： (相对路径, 条目对象)。
/// 兼容两种布局：顶层直接是文件条目，或外面再包一层（如 appid / "0"）。
pub fn extract_file_entries(root: &Node) -> Vec<(String, Node)> {
    fn looks_like_file_entry(n: &Node) -> bool {
        n.get("root").is_some()
            && (n.get("sha").is_some() || n.get("size").is_some() || n.get("syncstate").is_some())
    }

    let mut out = Vec::new();
    for (k, v) in &root.entries {
        if let Value::Obj(n) = v {
            if looks_like_file_entry(n) {
                out.push((k.clone(), n.clone()));
            } else {
                for (k2, v2) in &n.entries {
                    if let Value::Obj(n2) = v2 {
                        if looks_like_file_entry(n2) {
                            out.push((k2.clone(), n2.clone()));
                        }
                    }
                }
            }
        }
    }
    out
}
