#![allow(clippy::unwrap_used)]
use super::*;

/// 把一段 YAML 文本经 `dumps(loads(t))` 往返,断言**逐字节**等于 Python golden。
/// 这是 §4.2 双跑对该方言的字节锁:golden 由
/// `PYTHONPATH=src python3 -c "from team_agent.simple_yaml import loads,dumps; ..."` 取得。
fn roundtrip_eq(input: &str, golden: &str) {
    let v = loads(input).unwrap();
    assert_eq!(dumps(&v), golden, "roundtrip mismatch for input:\n{input}");
}

// === 真实语料:examples/team.spec.yaml 的 dumps(loads(t)) golden(逐字节) ===
#[test]
fn real_spec_yaml_roundtrip_matches_python_golden() {
    let input = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/model/testdata/team.spec.yaml"
    ));
    let golden = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/model/testdata/team.spec.golden.yaml"
    ));
    let v = loads(input).unwrap();
    assert_eq!(dumps(&v), golden);
}

// === 富边角 fixture:覆盖 ast.literal_eval 半成功(true/null 非法 → 整列退字符串)、
//     深层嵌套 map、list-of-map 含内嵌 list/map、block scalar 含空行 等 ===
#[test]
fn rich_edge_fixture_roundtrip_matches_python_golden() {
    let input = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/model/testdata/fuzz.yaml"
    ));
    let golden = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/model/testdata/fuzz.golden.yaml"
    ));
    let v = loads(input).unwrap();
    assert_eq!(dumps(&v), golden);
}

// === SKILL.md front-matter 块(`---` 之间那段)的 loads/dumps ===
#[test]
fn skill_front_matter_roundtrip() {
    // 取自 skills/team-agent/SKILL.md 的 front matter。
    let input = "name: team-agent\ndescription: Use only when the user explicitly asks to start, operate, inspect, shutdown, or restart a Team Agent team. Treat the team-agent CLI as a sealed appliance.\n";
    let golden = "name: \"team-agent\"\ndescription: \"Use only when the user explicitly asks to start, operate, inspect, shutdown, or restart a Team Agent team. Treat the team-agent CLI as a sealed appliance.\"\n";
    roundtrip_eq(input, golden);
}

// === 方言怪癖逐一对拍(每个 golden 来自 Python 真相源)===

#[test]
fn block_scalar_basic() {
    roundtrip_eq(
        "msg: |\n  line one\n  line two\n",
        "msg: |\n  line one\n  line two\n",
    );
}

#[test]
fn block_scalar_blank_line_becomes_padded() {
    // 空行在 dumps 时产生仅含 pad 的行(尾随空格)—— Python 行为,必须复刻。
    roundtrip_eq("msg: |\n  a\n\n  b\n", "msg: |\n  a\n  \n  b\n");
}

#[test]
fn inline_unquoted_list_stays_string() {
    // ast.literal_eval('[a, b, c]') 失败 → 原样字符串。
    roundtrip_eq("tools: [a, b, c]\n", "tools: \"[a, b, c]\"\n");
}

#[test]
fn inline_quoted_list_becomes_list() {
    roundtrip_eq("x: ['a', 'b']\n", "x:\n  - \"a\"\n  - \"b\"\n");
}

#[test]
fn ints_canonicalize() {
    // 007 -> 7,-5 保号。
    roundtrip_eq("a: 1\nb: -5\nc: 007\n", "a: 1\nb: -5\nc: 7\n");
}

#[test]
fn int_plus_and_underscore() {
    roundtrip_eq("a: +3\nb: 1_000\n", "a: 3\nb: 1000\n");
}

#[test]
fn non_ints_stay_strings() {
    roundtrip_eq(
        "a: 1.5\nb: 0x1f\nc: 1e3\nd: _1\n",
        "a: \"1.5\"\nb: \"0x1f\"\nc: \"1e3\"\nd: \"_1\"\n",
    );
}

#[test]
fn bools_and_nulls() {
    roundtrip_eq(
        "a: true\nb: False\nc: NULL\nd: ~\n",
        "a: true\nb: false\nc: null\nd: null\n",
    );
}

#[test]
fn empty_collections() {
    roundtrip_eq("a: {}\nb: []\n", "a: {}\nb: []\n");
}

#[test]
fn quoted_strings_unwrap() {
    roundtrip_eq("a: \"hello\"\nb: 'world'\n", "a: \"hello\"\nb: \"world\"\n");
}

#[test]
fn nested_map() {
    roundtrip_eq("a:\n  b:\n    c: 1\n", "a:\n  b:\n    c: 1\n");
}

#[test]
fn list_of_maps() {
    roundtrip_eq(
        "items:\n  - id: x\n    val: 1\n  - id: y\n    val: 2\n",
        "items:\n  - id: \"x\"\n    val: 1\n  - id: \"y\"\n    val: 2\n",
    );
}

#[test]
fn scalar_list_mixed() {
    roundtrip_eq(
        "a:\n  - 1\n  - two\n  - true\n",
        "a:\n  - 1\n  - \"two\"\n  - true\n",
    );
}

#[test]
fn empty_value_is_null() {
    roundtrip_eq("a:\nb: 2\n", "a: null\nb: 2\n");
}

#[test]
fn inline_comment_not_stripped() {
    // 整行注释被跳过;行内 `#` 不被当注释,留在值里。
    roundtrip_eq(
        "# top\na: 1  # inline?\nb: 2\n",
        "a: \"1  # inline?\"\nb: 2\n",
    );
}

#[test]
fn key_no_space_after_colon() {
    roundtrip_eq("a:1\n", "a: 1\n");
}

#[test]
fn special_chars_json_escaped() {
    roundtrip_eq("a: he said \"hi\"\n", "a: \"he said \\\"hi\\\"\"\n");
}

#[test]
fn unicode_preserved() {
    roundtrip_eq("name: café ☕\n", "name: \"café ☕\"\n");
}

#[test]
fn list_item_value_has_colon_and_space() {
    // foo: bar baz —— item_text 看起来是 key-value(key=foo 合法),value = "bar baz"。
    roundtrip_eq("a:\n  - foo: bar baz\n", "a:\n  - foo: \"bar baz\"\n");
}

// === JSON 顶层路径(`{`/`[` 开头 → json.loads,保留键序)===

#[test]
fn json_object_path_preserves_key_order() {
    roundtrip_eq("{\"a\": 1, \"b\": [1,2]}", "a: 1\nb:\n  - 1\n  - 2\n");
}

#[test]
fn json_array_path() {
    roundtrip_eq("[1, 2, 3]", "- 1\n- 2\n- 3\n");
}

#[test]
fn json_leading_whitespace() {
    roundtrip_eq("   {\"a\": 1}", "a: 1\n");
}

#[test]
fn json_float_becomes_quoted_string() {
    // json.loads -> float 1.5; dumps -> json.dumps(str(1.5)) = "1.5"。
    roundtrip_eq("{\"a\": 1.5}", "a: \"1.5\"\n");
}

#[test]
fn json_string_with_newline_becomes_block_scalar() {
    roundtrip_eq("{\"x\": \"with\\nnewline\"}", "x: |\n  with\n  newline\n");
}

#[test]
fn json_empty_object_and_array_top_level() {
    // 顶层 {} / [] → _dump 返回空 → "\n"。
    roundtrip_eq("{}", "\n");
    roundtrip_eq("[]", "\n");
}

// === dumps-only 结构(直接构造 Value,golden 来自 Python dumps)===

#[test]
fn dump_nested_list_in_list() {
    let v = Value::Map(vec![(
        "a".to_string(),
        Value::List(vec![
            Value::List(vec![Value::Int(1), Value::Int(2)]),
            Value::List(vec![Value::Int(3)]),
        ]),
    )]);
    assert_eq!(dumps(&v), "a:\n  -\n    - 1\n    - 2\n  -\n    - 3\n");
}

#[test]
fn dump_mixed_list() {
    let v = Value::List(vec![
        Value::Int(1),
        Value::Str("two".to_string()),
        Value::List(vec![Value::Int(3), Value::Int(4)]),
        Value::Map(vec![("k".to_string(), Value::Str("v".to_string()))]),
        Value::Map(Vec::new()),
        Value::List(Vec::new()),
    ]);
    assert_eq!(
        dumps(&v),
        "- 1\n- \"two\"\n-\n  - 3\n  - 4\n- k: \"v\"\n- {}\n-\n"
    );
}

#[test]
fn dump_multiline_string_in_list_item_is_not_block_scalar() {
    // 关键不对称:list-of-dict 上下文**不**对多行字符串走 `|`。
    let v = Value::Map(vec![(
        "items".to_string(),
        Value::List(vec![Value::Map(vec![(
            "prompt".to_string(),
            Value::Str("line1\nline2".to_string()),
        )])]),
    )]);
    assert_eq!(dumps(&v), "items:\n  - prompt: \"line1\\nline2\"\n");
}

#[test]
fn dump_deep_map_in_list_uses_indent_plus_4() {
    let v = Value::Map(vec![(
        "items".to_string(),
        Value::List(vec![Value::Map(vec![
            (
                "a".to_string(),
                Value::Map(vec![("b".to_string(), Value::Int(1))]),
            ),
            ("c".to_string(), Value::Int(2)),
        ])]),
    )]);
    assert_eq!(dumps(&v), "items:\n  - a:\n      b: 1\n    c: 2\n");
}

#[test]
fn dump_string_that_looks_like_int_or_bool_is_quoted() {
    let v = Value::Map(vec![
        ("a".to_string(), Value::Str("5".to_string())),
        ("b".to_string(), Value::Str("true".to_string())),
    ]);
    assert_eq!(dumps(&v), "a: \"5\"\nb: \"true\"\n");
}

#[test]
fn dump_string_single_newline_only() {
    // "\n".rstrip("\n") = "" -> splitlines() = [] -> 只有 "a: |"。
    let v = Value::Map(vec![("a".to_string(), Value::Str("\n".to_string()))]);
    assert_eq!(dumps(&v), "a: |\n");
}

// === loads 错误路径(与 Python raise 对齐)===

#[test]
fn unexpected_indentation_errors() {
    assert!(loads("a: 1\n  b: 2\n").is_err());
}

#[test]
fn missing_colon_errors() {
    assert!(loads("just a string\n").is_err());
}

#[test]
fn json_escape_matches_python_for_control_and_specials() {
    // 锁住 serde_json 字符串转义 == Python json.dumps(ensure_ascii=False)。
    assert_eq!(json_quote("a\"b"), "\"a\\\"b\"");
    assert_eq!(json_quote("a\\b"), "\"a\\\\b\"");
    assert_eq!(json_quote("a\nb"), "\"a\\nb\"");
    assert_eq!(json_quote("tab\there"), "\"tab\\there\"");
    assert_eq!(json_quote("a\rb"), "\"a\\rb\"");
    assert_eq!(json_quote("café"), "\"café\"");
    assert_eq!(json_quote("\u{0001}ctrl"), "\"\\u0001ctrl\"");
    assert_eq!(json_quote("slash/here"), "\"slash/here\"");
    assert_eq!(json_quote("back\u{0008}space"), "\"back\\bspace\"");
    assert_eq!(json_quote("form\u{000c}feed"), "\"form\\ffeed\"");
}
