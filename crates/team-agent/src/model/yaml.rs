//! step 2 · model — `simple_yaml` 方言移植(真相源 `src/team_agent/simple_yaml.py`)。
//!
//! Team Agent **不**用标准 YAML:它有一份零依赖、字节级特判的子集 `loads`/`dumps`,
//! spec / role-doc front-matter / team_state 全靠它解析与回写。**禁用 serde_yaml**——
//! 标准 YAML 在空白 / 引号 / `|` block scalar / `[...]`(`ast.literal_eval`)/ int 解析 /
//! dict-vs-list-上下文 dump 的诸多怪癖上都会漂移 → spec 解析不一致 → 下游全错(§7)。
//! 这里移植的是**那份方言本身**,golden 字节由 Python 真相源双跑锁死(§4.2)。
//!
//! 与 Python `Any` 语义对齐的 [`Value`]:
//! - `dict` 保持插入顺序 → `Map(Vec<(String, Value)>)`(非 `BTreeMap`,否则 JSON 路径乱序)。
//! - int 用 `i64`(真相源 Python 任意精度;实际语料只有 version/priority/timeout 等小整数)。
//! - float 仅经 JSON 顶层路径出现(`{`/`[` 开头 → `json.loads`),`dumps` 时按 Python
//!   `json.dumps(str(f))` 走带引号字符串分支。
//!
//! §10:纯层无 panic —— `loads` 校验失败返 [`ModelError`];`dumps` 不会失败。

use serde::de::{self, Deserialize, Deserializer, MapAccess, SeqAccess, Visitor};

use crate::model::errors::ModelError;

/// 与 Python `simple_yaml` 的 `Any` 一一对应的动态值。
///
/// `Map` 用有序 `Vec<(String, Value)>` 复刻 Python dict 的插入顺序(`dumps` 按此顺序输出,
/// 字节对拍依赖它)。
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    Int(i64),
    /// 仅经 JSON 顶层路径产生(`loads("{...}")`)。`dumps` 走 `json.dumps(str(f))` 分支。
    Float(f64),
    Str(String),
    List(Vec<Value>),
    /// 有序键值对(复刻 Python dict 插入顺序)。
    Map(Vec<(String, Value)>),
}

impl Value {
    fn is_empty_list(&self) -> bool {
        matches!(self, Value::List(v) if v.is_empty())
    }
    fn is_empty_map(&self) -> bool {
        matches!(self, Value::Map(m) if m.is_empty())
    }
    fn is_collection(&self) -> bool {
        matches!(self, Value::List(_) | Value::Map(_))
    }

    // --- 公开访问器(spec/compiler/routing/state 遍历 spec Value 用;对应 Python dict/list 取值)---

    /// `Str` → `&str`,否则 `None`。
    pub fn as_str(&self) -> Option<&str> {
        if let Value::Str(s) = self {
            Some(s)
        } else {
            None
        }
    }
    /// `Map` 的有序键值对切片,否则 `None`。
    pub fn as_map(&self) -> Option<&[(String, Value)]> {
        if let Value::Map(m) = self {
            Some(m)
        } else {
            None
        }
    }
    /// `List` 切片,否则 `None`。
    pub fn as_list(&self) -> Option<&[Value]> {
        if let Value::List(l) = self {
            Some(l)
        } else {
            None
        }
    }
    /// 是否 `Map`(对应 Python `isinstance(x, dict)`)。
    pub fn is_map(&self) -> bool {
        matches!(self, Value::Map(_))
    }
    /// `Int` → `i64`,否则 `None`。
    pub fn as_i64(&self) -> Option<i64> {
        if let Value::Int(i) = self {
            Some(*i)
        } else {
            None
        }
    }
    /// dict get:首个匹配 key 的值(`insert_ordered` 已去重 → 首即唯一)。非 Map → `None`。
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.as_map()?.iter().find(|(k, _)| k == key).map(|(_, v)| v)
    }
    /// Python 真值语义:None/Null/false/0/""/空集 → false。
    pub fn is_truthy(&self) -> bool {
        match self {
            Value::Null => false,
            Value::Bool(b) => *b,
            Value::Int(i) => *i != 0,
            Value::Float(f) => *f != 0.0,
            Value::Str(s) => !s.is_empty(),
            Value::List(l) => !l.is_empty(),
            Value::Map(m) => !m.is_empty(),
        }
    }
}

// ---------------------------------------------------------------------------
// loads
// ---------------------------------------------------------------------------

/// 移植 `simple_yaml.loads`。
///
/// `text.lstrip()` 以 `{`/`[` 开头 → 整段走 `json.loads`(JSON 子集,保持键序)。
/// 否则按 Python 的缩进块方言解析。无法解析返 [`ModelError::Validation`]。
pub fn loads(text: &str) -> Result<Value, ModelError> {
    let stripped = text.trim_start();
    if stripped.starts_with('{') || stripped.starts_with('[') {
        return loads_json(text);
    }
    let lines: Vec<&str> = split_lines(text);
    let p = Parser { lines: &lines };
    let (value, mut index) = p.parse_block(0, 0)?;
    while index < lines.len() && !content(lines[index]) {
        index += 1;
    }
    if index != lines.len() {
        return Err(ModelError::Validation(format!(
            "unexpected content at line {}: {}",
            index + 1,
            lines[index]
        )));
    }
    Ok(value)
}

/// Python `str.splitlines()`:无尾随空行(末尾 `\n` 不产生空元素),`\r\n`/`\r` 也算行界。
/// 真相源语料是 `\n` 结尾的 UTF-8 文本;这里按 `\n` 切并剥掉每行尾随 `\r`,与 Python 对齐。
fn split_lines(text: &str) -> Vec<&str> {
    if text.is_empty() {
        return Vec::new();
    }
    let mut out: Vec<&str> = Vec::new();
    for part in text.split('\n') {
        out.push(part.strip_suffix('\r').unwrap_or(part));
    }
    // split('\n') 在尾随 '\n' 时产生一个末尾空串 —— Python splitlines 不会,去掉它。
    if text.ends_with('\n') {
        out.pop();
    }
    out
}

fn loads_json(text: &str) -> Result<Value, ModelError> {
    let mut de = serde_json::Deserializer::from_str(text);
    let value =
        Value::deserialize(&mut de).map_err(|e| ModelError::Validation(format!("json: {e}")))?;
    de.end()
        .map_err(|e| ModelError::Validation(format!("json: {e}")))?;
    Ok(value)
}

struct Parser<'a> {
    lines: &'a [&'a str],
}

impl<'a> Parser<'a> {
    fn parse_block(&self, mut index: usize, indent: usize) -> Result<(Value, usize), ModelError> {
        index = skip_blank(self.lines, index);
        if index >= self.lines.len() {
            return Ok((Value::Null, index));
        }
        let current_indent = line_indent(self.lines[index]);
        if current_indent < indent {
            return Ok((Value::Null, index));
        }
        if self.lines[index].trim().starts_with("- ") {
            self.parse_list(index, current_indent)
        } else {
            self.parse_dict(index, current_indent)
        }
    }

    fn parse_dict(&self, mut index: usize, indent: usize) -> Result<(Value, usize), ModelError> {
        let mut obj: Vec<(String, Value)> = Vec::new();
        while index < self.lines.len() {
            if !content(self.lines[index]) {
                index += 1;
                continue;
            }
            let line_ind = line_indent(self.lines[index]);
            if line_ind < indent {
                break;
            }
            if line_ind > indent {
                return Err(ModelError::Validation(format!(
                    "unexpected indentation at line {}: {}",
                    index + 1,
                    self.lines[index]
                )));
            }
            let stripped = self.lines[index].trim();
            if stripped.starts_with("- ") {
                break;
            }
            let (key, raw) = split_key_value(stripped, index)?;
            let value = if raw == "|" {
                let (v, ni) = self.parse_block_scalar(index + 1, indent + 2);
                index = ni;
                v
            } else if raw.is_empty() {
                let (v, ni) = self.parse_block(index + 1, indent + 2)?;
                index = ni;
                v
            } else {
                index += 1;
                parse_scalar(&raw)
            };
            insert_ordered(&mut obj, key, value);
        }
        Ok((Value::Map(obj), index))
    }

    fn parse_list(&self, mut index: usize, indent: usize) -> Result<(Value, usize), ModelError> {
        let mut items: Vec<Value> = Vec::new();
        while index < self.lines.len() {
            if !content(self.lines[index]) {
                index += 1;
                continue;
            }
            let line_ind = line_indent(self.lines[index]);
            if line_ind < indent {
                break;
            }
            if line_ind != indent {
                return Err(ModelError::Validation(format!(
                    "unexpected list indentation at line {}: {}",
                    index + 1,
                    self.lines[index]
                )));
            }
            let stripped = self.lines[index].trim();
            if !stripped.starts_with("- ") {
                break;
            }
            let item_text = stripped[2..].trim();
            if item_text.is_empty() {
                let (value, ni) = self.parse_block(index + 1, indent + 2)?;
                items.push(value);
                index = ni;
                continue;
            }
            if looks_like_key_value(item_text) {
                let (key, raw) = split_key_value(item_text, index)?;
                let mut item: Vec<(String, Value)> = Vec::new();
                let (value, mut next_index) = if raw == "|" {
                    self.parse_block_scalar(index + 1, indent + 2)
                } else if raw.is_empty() {
                    self.parse_block(index + 1, indent + 2)?
                } else {
                    (parse_scalar(&raw), index + 1)
                };
                insert_ordered(&mut item, key, value);
                if next_index < self.lines.len()
                    && line_indent(self.lines[next_index]) == indent + 2
                {
                    let (extra, ni) = self.parse_dict(next_index, indent + 2)?;
                    if let Value::Map(pairs) = extra {
                        for (k, v) in pairs {
                            insert_ordered(&mut item, k, v);
                        }
                    }
                    next_index = ni;
                }
                items.push(Value::Map(item));
                index = next_index;
            } else {
                items.push(parse_scalar(item_text));
                index += 1;
            }
        }
        Ok((Value::List(items), index))
    }

    /// `|` block scalar:取 `indent` 之后的内容,空行记为 `""`,末尾 `rstrip()` + `"\n"`。
    fn parse_block_scalar(&self, mut index: usize, indent: usize) -> (Value, usize) {
        let mut block: Vec<String> = Vec::new();
        while index < self.lines.len() {
            if self.lines[index].trim().is_empty() {
                block.push(String::new());
                index += 1;
                continue;
            }
            let line_ind = line_indent(self.lines[index]);
            if line_ind < indent {
                break;
            }
            block.push(slice_from_byte(self.lines[index], indent));
            index += 1;
        }
        let joined = block.join("\n");
        (Value::Str(format!("{}\n", py_rstrip(&joined))), index)
    }
}

/// Python dict 赋值语义:键已存在则覆盖**值**但保留原插入位置。
fn insert_ordered(map: &mut Vec<(String, Value)>, key: String, value: Value) {
    if let Some(slot) = map.iter_mut().find(|(k, _)| *k == key) {
        slot.1 = value;
    } else {
        map.push((key, value));
    }
}

/// 移植 `_parse_scalar`。
fn parse_scalar(raw: &str) -> Value {
    match raw {
        "null" | "Null" | "NULL" | "~" => return Value::Null,
        "true" | "True" | "TRUE" => return Value::Bool(true),
        "false" | "False" | "FALSE" => return Value::Bool(false),
        _ => {}
    }
    if let Some(n) = py_int(raw) {
        return Value::Int(n);
    }
    if raw.starts_with('[') && raw.ends_with(']') {
        // ast.literal_eval(raw):成功 → list,失败(SyntaxError/ValueError)→ 原样 str。
        return match literal_eval_list(raw) {
            Some(list) => list,
            None => Value::Str(raw.to_string()),
        };
    }
    if raw == "{}" {
        return Value::Map(Vec::new());
    }
    if (raw.starts_with('"') && raw.ends_with('"')) || (raw.starts_with('\'') && raw.ends_with('\''))
    {
        if raw.len() < 2 {
            // 单个引号字符:不是合法的成对引号,落到末尾 return raw。
            return Value::Str(raw.to_string());
        }
        return match literal_eval_quoted(raw) {
            Some(s) => Value::Str(s),
            // ast.literal_eval 失败 → Python 退化为 raw[1:-1](剥首尾各一字节)。
            None => Value::Str(strip_one_each_end(raw)),
        };
    }
    Value::Str(raw.to_string())
}

/// 移植 Python `int(raw)`:十进制,允许前导 `+`/`-`、内部单下划线(不可前/尾/连续),
/// 拒绝空、浮点、十六进制、`e` 记数等。失败返 `None`(→ 落到字符串)。
/// 任意精度退化为 `i64`:语料只有小整数,超出 `i64` 时返 `None`(回退字符串)以免 panic。
fn py_int(raw: &str) -> Option<i64> {
    let bytes = raw.as_bytes();
    if bytes.is_empty() {
        return None;
    }
    let mut i = 0usize;
    let neg = match bytes[0] {
        b'+' => {
            i = 1;
            false
        }
        b'-' => {
            i = 1;
            true
        }
        _ => false,
    };
    let digits = &raw[i..];
    if digits.is_empty() {
        return None;
    }
    // 下划线规则:不能开头/结尾,不能连续,两侧必须是数字。
    let db = digits.as_bytes();
    let mut prev_underscore = false;
    for (idx, &c) in db.iter().enumerate() {
        if c == b'_' {
            if idx == 0 || idx == db.len() - 1 || prev_underscore {
                return None;
            }
            prev_underscore = true;
        } else if c.is_ascii_digit() {
            prev_underscore = false;
        } else {
            return None;
        }
    }
    let cleaned: String = digits.chars().filter(|&c| c != '_').collect();
    let magnitude: i64 = cleaned.parse().ok()?;
    Some(if neg { -magnitude } else { magnitude })
}

/// `ast.literal_eval` 子集,仅覆盖 `[...]` 中由标量(int / 引号字符串 / bool / None)
/// 组成的列表 —— 这是真相源唯一会命中此路径的形态(如 `tools: ['a', 'b']`)。
/// 任何无法以此子集解析的输入返 `None`(→ 调用方回退为原始字符串,与 Python 一致)。
fn literal_eval_list(raw: &str) -> Option<Value> {
    let inner = &raw[1..raw.len() - 1];
    let inner_trim = inner.trim();
    if inner_trim.is_empty() {
        return Some(Value::List(Vec::new()));
    }
    let mut items: Vec<Value> = Vec::new();
    for part in split_top_level_commas(inner_trim)? {
        items.push(literal_eval_atom(part.trim())?);
    }
    Some(Value::List(items))
}

/// 按顶层逗号切分(不进入引号 / 不处理嵌套,够覆盖语料中的扁平列表)。
/// 末尾允许一个尾随逗号(Python 列表字面量允许)。
fn split_top_level_commas(s: &str) -> Option<Vec<&str>> {
    let bytes = s.as_bytes();
    let mut parts: Vec<&str> = Vec::new();
    let mut start = 0usize;
    let mut quote: Option<u8> = None;
    let mut i = 0usize;
    while i < bytes.len() {
        let c = bytes[i];
        match quote {
            Some(q) => {
                if c == q {
                    quote = None;
                }
            }
            None => {
                if c == b'\'' || c == b'"' {
                    quote = Some(c);
                } else if c == b'[' || c == b']' || c == b'{' || c == b'}' {
                    // 嵌套结构超出本子集 → 让 ast 在 Python 里也可能成功但这里保守失败。
                    return None;
                } else if c == b',' {
                    parts.push(&s[start..i]);
                    start = i + 1;
                }
            }
        }
        i += 1;
    }
    if quote.is_some() {
        return None;
    }
    let tail = s[start..].trim();
    if !tail.is_empty() {
        parts.push(&s[start..]);
    }
    Some(parts)
}

fn literal_eval_atom(tok: &str) -> Option<Value> {
    if tok.is_empty() {
        return None;
    }
    match tok {
        "None" => return Some(Value::Null),
        "True" => return Some(Value::Bool(true)),
        "False" => return Some(Value::Bool(false)),
        _ => {}
    }
    let bytes = tok.as_bytes();
    let first = bytes[0];
    if (first == b'"' || first == b'\'') && tok.len() >= 2 && bytes[bytes.len() - 1] == first {
        return literal_eval_quoted(tok).map(Value::Str);
    }
    // Python int literal(literal_eval 允许下划线/正负号)。
    if let Some(n) = py_int(tok) {
        return Some(Value::Int(n));
    }
    None
}

/// `ast.literal_eval` 一个引号字符串字面量。支持 `\n \t \" \' \\` 等常见转义;
/// 解析失败返 `None`。
fn literal_eval_quoted(raw: &str) -> Option<String> {
    let bytes = raw.as_bytes();
    if raw.len() < 2 {
        return None;
    }
    let q = bytes[0];
    if (q != b'"' && q != b'\'') || bytes[raw.len() - 1] != q {
        return None;
    }
    let inner = &raw[1..raw.len() - 1];
    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some('r') => out.push('\r'),
                Some('\\') => out.push('\\'),
                Some('\'') => out.push('\''),
                Some('"') => out.push('"'),
                Some('0') => out.push('\0'),
                Some(other) => {
                    out.push('\\');
                    out.push(other);
                }
                None => return None,
            }
        } else if c == q as char {
            // 未转义的同种引号出现在内部 → 非法字面量。
            return None;
        } else {
            out.push(c);
        }
    }
    Some(out)
}

/// `raw[1:-1]`:按字节剥掉首尾各一字节(Python 切片语义)。语料中引号为 ASCII。
fn strip_one_each_end(raw: &str) -> String {
    let mut chars = raw.chars();
    chars.next();
    chars.next_back();
    chars.as_str().to_string()
}

// ---------------------------------------------------------------------------
// dumps
// ---------------------------------------------------------------------------

/// 移植 `simple_yaml.dumps`:`"\n".join(_dump(value, indent)) + "\n"`。
pub fn dumps(value: &Value) -> String {
    let lines = dump(value, 0);
    let mut out = lines.join("\n");
    out.push('\n');
    out
}

fn dump(value: &Value, indent: usize) -> Vec<String> {
    let pad = " ".repeat(indent);
    match value {
        Value::Map(pairs) => {
            let mut lines: Vec<String> = Vec::new();
            for (key, item) in pairs {
                if item.is_empty_list() {
                    lines.push(format!("{pad}{key}: []"));
                } else if item.is_empty_map() {
                    lines.push(format!("{pad}{key}: {{}}"));
                } else if item.is_collection() {
                    lines.push(format!("{pad}{key}:"));
                    lines.extend(dump(item, indent + 2));
                } else if let Value::Str(s) = item {
                    if s.contains('\n') {
                        lines.push(format!("{pad}{key}: |"));
                        for block_line in py_splitlines(py_rstrip_newlines(s)) {
                            lines.push(format!("{pad}  {block_line}"));
                        }
                    } else {
                        lines.push(format!("{pad}{key}: {}", format_scalar(item)));
                    }
                } else {
                    lines.push(format!("{pad}{key}: {}", format_scalar(item)));
                }
            }
            lines
        }
        Value::List(items) => {
            let mut lines: Vec<String> = Vec::new();
            for item in items {
                match item {
                    Value::Map(pairs) => {
                        if pairs.is_empty() {
                            lines.push(format!("{pad}- {{}}"));
                            continue;
                        }
                        let mut first = true;
                        for (key, child) in pairs {
                            let prefix = if first { "- " } else { "  " };
                            if child.is_empty_list() {
                                lines.push(format!("{pad}{prefix}{key}: []"));
                            } else if child.is_empty_map() {
                                lines.push(format!("{pad}{prefix}{key}: {{}}"));
                            } else if child.is_collection() {
                                lines.push(format!("{pad}{prefix}{key}:"));
                                lines.extend(dump(child, indent + 4));
                            } else {
                                // 注意:此分支**不**对多行字符串做 `|`,与 Python 一致。
                                lines.push(format!(
                                    "{pad}{prefix}{key}: {}",
                                    format_scalar(child)
                                ));
                            }
                            first = false;
                        }
                    }
                    Value::List(_) => {
                        lines.push(format!("{pad}-"));
                        lines.extend(dump(item, indent + 2));
                    }
                    _ => {
                        lines.push(format!("{pad}- {}", format_scalar(item)));
                    }
                }
            }
            lines
        }
        _ => vec![format!("{pad}{}", format_scalar(value))],
    }
}

/// 移植 `_format_scalar`。非 None/bool/int → `json.dumps(str(value), ensure_ascii=False)`。
fn format_scalar(value: &Value) -> String {
    match value {
        Value::Null => "null".to_string(),
        Value::Bool(true) => "true".to_string(),
        Value::Bool(false) => "false".to_string(),
        Value::Int(n) => n.to_string(),
        Value::Float(f) => json_quote(&py_float_str(*f)),
        Value::Str(s) => json_quote(s),
        // collection / empty 由 dump() 的上层分支拦截;此处理论不可达,保守按 str 化。
        Value::List(_) | Value::Map(_) => json_quote("[object]"),
    }
}

/// `json.dumps(s, ensure_ascii=False)`:Rust `serde_json` 的字符串转义与 Python 对齐
/// (`\b \f \n \r \t`、控制字符 `\uXXXX`、`/` 不转义、非 ASCII 原样保留)。
fn json_quote(s: &str) -> String {
    // serde_json::to_string 对 &str 不会失败(无 IO),但 §10 禁 unwrap:回退为手工引号。
    serde_json::to_string(s).unwrap_or_else(|_| format!("\"{s}\""))
}

/// Python `str(float)`:对本语料(仅 JSON 路径偶发)足够。整数值浮点带 `.0`。
fn py_float_str(f: f64) -> String {
    if f.is_nan() {
        return "nan".to_string();
    }
    if f.is_infinite() {
        return if f > 0.0 { "inf" } else { "-inf" }.to_string();
    }
    if f == f.trunc() && f.abs() < 1e16 {
        return format!("{f:.1}");
    }
    // Rust 默认 `{}` 浮点格式化给最短往返表示,与 Python repr/str 在常见小数上一致。
    format!("{f}")
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

/// `_content`:非空且非 `#` 开头(`strip()` 后)。
fn content(line: &str) -> bool {
    let s = line.trim();
    !s.is_empty() && !s.starts_with('#')
}

fn skip_blank(lines: &[&str], mut index: usize) -> usize {
    while index < lines.len() && !content(lines[index]) {
        index += 1;
    }
    index
}

/// `_indent`:前导**空格**数(`lstrip(" ")` 只剥空格,不含 tab)。
fn line_indent(line: &str) -> usize {
    line.len() - line.trim_start_matches(' ').len()
}

/// `_split_key_value`:无 `:` 报错;否则按首个 `:` 切,两侧 strip。
fn split_key_value(stripped: &str, index: usize) -> Result<(String, String), ModelError> {
    match stripped.split_once(':') {
        None => Err(ModelError::Validation(format!(
            "expected key: value at line {}",
            index + 1
        ))),
        Some((key, raw)) => Ok((key.trim().to_string(), raw.trim().to_string())),
    }
}

/// `_looks_like_key_value`:含 `:`,且首个 `:` 之前的 key 非空且只由
/// 字母数字 / `_` / `-` 组成。
fn looks_like_key_value(text: &str) -> bool {
    match text.split_once(':') {
        None => false,
        Some((key, _)) => {
            !key.is_empty()
                && key
                    .chars()
                    .all(|c| c.is_alphanumeric() || c == '_' || c == '-')
        }
    }
}

/// Python `str.rstrip()`(无参):剥尾随空白(空格 / `\t` / `\n` / `\r` / `\f` / `\v` 等)。
fn py_rstrip(s: &str) -> &str {
    s.trim_end_matches(|c: char| c.is_whitespace())
}

/// Python `str.rstrip("\n")`:仅剥尾随换行符。
fn py_rstrip_newlines(s: &str) -> &str {
    s.trim_end_matches('\n')
}

/// Python `str.splitlines()`(用于 `_dump` 的 block scalar 拆行)。
fn py_splitlines(s: &str) -> Vec<&str> {
    if s.is_empty() {
        return Vec::new();
    }
    let mut out: Vec<&str> = Vec::new();
    for part in s.split('\n') {
        out.push(part.strip_suffix('\r').unwrap_or(part));
    }
    if s.ends_with('\n') {
        out.pop();
    }
    out
}

/// 取 `line[indent:]`(字节切片,复刻 Python 切片)。语料缩进与正文为 ASCII/UTF-8,
/// 缩进位为空格 → `indent` 落在字符边界。越界则返空串。
fn slice_from_byte(line: &str, indent: usize) -> String {
    if indent >= line.len() {
        return String::new();
    }
    if line.is_char_boundary(indent) {
        line[indent..].to_string()
    } else {
        // 不在字符边界(理论上不会发生,因缩进恒为空格):保守按字符跳过 indent 个码点。
        line.chars().skip(indent).collect()
    }
}

// ---------------------------------------------------------------------------
// JSON 顶层路径:用 serde 访问者把 JSON 直接读成有序 Value(保留键序)
// ---------------------------------------------------------------------------

impl<'de> Deserialize<'de> for Value {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct ValueVisitor;

        impl<'de> Visitor<'de> for ValueVisitor {
            type Value = Value;

            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("a JSON value")
            }

            fn visit_unit<E>(self) -> Result<Value, E> {
                Ok(Value::Null)
            }
            fn visit_none<E>(self) -> Result<Value, E> {
                Ok(Value::Null)
            }
            fn visit_bool<E>(self, v: bool) -> Result<Value, E> {
                Ok(Value::Bool(v))
            }
            fn visit_i64<E>(self, v: i64) -> Result<Value, E> {
                Ok(Value::Int(v))
            }
            fn visit_u64<E>(self, v: u64) -> Result<Value, E>
            where
                E: de::Error,
            {
                i64::try_from(v)
                    .map(Value::Int)
                    .map_err(|_| de::Error::custom("integer out of i64 range"))
            }
            fn visit_f64<E>(self, v: f64) -> Result<Value, E> {
                Ok(Value::Float(v))
            }
            fn visit_str<E>(self, v: &str) -> Result<Value, E> {
                Ok(Value::Str(v.to_string()))
            }
            fn visit_string<E>(self, v: String) -> Result<Value, E> {
                Ok(Value::Str(v))
            }
            fn visit_seq<A>(self, mut seq: A) -> Result<Value, A::Error>
            where
                A: SeqAccess<'de>,
            {
                let mut items: Vec<Value> = Vec::new();
                while let Some(v) = seq.next_element()? {
                    items.push(v);
                }
                Ok(Value::List(items))
            }
            fn visit_map<A>(self, mut map: A) -> Result<Value, A::Error>
            where
                A: MapAccess<'de>,
            {
                // 访问者按源文档顺序遍历 → 保留 JSON 键序(serde_json 默认 BTreeMap
                // 会乱序,故此处不经 serde_json::Value)。
                let mut pairs: Vec<(String, Value)> = Vec::new();
                while let Some((k, v)) = map.next_entry::<String, Value>()? {
                    insert_ordered(&mut pairs, k, v);
                }
                Ok(Value::Map(pairs))
            }
        }

        deserializer.deserialize_any(ValueVisitor)
    }
}

#[cfg(test)]
mod tests;
