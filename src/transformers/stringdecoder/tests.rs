use super::arrays::StringArrayFinder;
use super::core::StringDecoder;
use super::decoder_finder::DecoderFunctionFinder;
use super::replacer::parse_index_str;
use crate::Deobfuscator;
use crate::context::{StringArray, StringArrayType};
use crate::deobfuscator::DeobfuscateOptions;
use crate::transformers::Transformer as _;
use std::sync::Arc;
use swc_common::GLOBALS;
use swc_ecma_ast::{Expr, Program};

fn deob_with_stringdecoder(code: &str) -> String {
    let deob = Deobfuscator::new();
    let options = DeobfuscateOptions {
        custom_transformers: Some(vec![Arc::new(StringDecoder::new())]),
        ..DeobfuscateOptions::default()
    };
    deob.deobfuscate_source(code, Some(options)).unwrap()
}

#[test]
fn stringdecoder_new() {
    let transformer = StringDecoder::new();
    assert_eq!(transformer.name(), "StringDecoder");
}

#[test]
fn base64_decode() {
    let charset = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/=";
    let result = StringDecoder::base64_decode(charset, "SGVsbG8=");
    assert_eq!(result, Some("Hello".to_owned()));
}

#[test]
fn base64_decode_world() {
    let charset = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/=";
    let result = StringDecoder::base64_decode(charset, "V29ybGQ=");
    assert_eq!(result, Some("World".to_owned()));
}

#[test]
fn base64_decode_no_padding() {
    let charset = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/=";
    // "Hi" without padding
    let result = StringDecoder::base64_decode(charset, "SGk");
    assert_eq!(result, Some("Hi".to_owned()));
}

#[test]
fn base91_decode() {
    let charset = r#"O#yxz27.D<RQZtISU{j0W"5:f)$L%]!9bA1E^*h`opKgruN8cns~Jadm3MP4>}XTqlB?vC|+w@/6FH=[G_V&eiYk;,("#;
    let result = StringDecoder::base91_decode(charset, "=Ic<A=O");
    assert_eq!(result, Some("Hello".to_owned()));
}

#[test]
fn rc4_decrypt() {
    let charset = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/=";
    // RC4 decryption requires specific encoded input
    // This is a basic test to ensure the function doesn't panic
    let result = StringDecoder::rc4_decrypt(charset, "SGVsbG8=", "key");
    assert!(result.is_some());
}

#[test]
fn string_array_finder_variable_form() {
    let deob = Deobfuscator::new();
    let code = r#"
var _0x1234 = ["hello", "world", "test", "foo", "bar"];
console.log(_0x1234[0]);
"#;
    // This tests that the parser doesn't crash on string arrays
    let result = deob.deobfuscate_source(code, None);
    result.unwrap();
}

#[test]
fn unused_string_array_removed() {
    let code = r#"
function _0x2237() {
  var _0x18c20a = ["a", "b", "c", "d", "e"];
  _0x2237 = function() { return _0x18c20a; };
  return _0x2237();
}
const ok = 1;
"#;
    let result = deob_with_stringdecoder(code);
    assert!(!result.contains("_0x2237"));
    assert!(!result.contains("_0x18c20a"));
    assert!(result.contains("const ok") || result.contains("var ok"));
}

#[test]
fn unused_string_array_with_alias_removed() {
    let code = r#"
function _0x2237() {
  var _0x18c20a = ["a", "b", "c", "d", "e"];
  _0x2237 = function() { return _0x18c20a; };
  return _0x2237();
}
var alias = _0x2237;
const ok = 1;
"#;
    let result = deob_with_stringdecoder(code);
    assert!(!result.contains("_0x2237"));
    assert!(!result.contains("_0x18c20a"));
    assert!(result.contains("const ok") || result.contains("var ok"));
}

#[test]
fn parse_index_str_hex() {
    let hex: i32 = 16;
    let neg_hex: i32 = -26;
    let dec: i32 = 42;
    let neg_dec: i32 = -7;
    assert_eq!(parse_index_str("0x10"), Some(hex));
    assert_eq!(parse_index_str("-0x1a"), Some(neg_hex));
    assert_eq!(parse_index_str("42"), Some(dec));
    assert_eq!(parse_index_str("-7"), Some(neg_dec));
}

#[test]
fn extract_string_array() {
    use swc_common::{FileName, SourceMap, sync::Lrc};
    use swc_ecma_parser::{EsSyntax, Parser, StringInput, Syntax};

    let cm: Lrc<SourceMap> = Lrc::default();
    let fm = cm.new_source_file(
        FileName::Custom("test.js".into()).into(),
        r#"["a", "b", "c"]"#,
    );

    let mut parser = Parser::new(
        Syntax::Es(EsSyntax::default()),
        StringInput::from(&*fm),
        None,
    );

    let expr = parser.parse_expr().unwrap();
    assert!(
        matches!(&*expr, Expr::Array(_)),
        "Expected array expression"
    );
    let Expr::Array(arr) = &*expr else {
        return;
    };
    let strings = StringArrayFinder::extract_string_array(arr).expect("expected string array");
    assert_eq!(strings.len(), 3);
    assert_eq!(strings.first().map(String::as_str), Some("a"));
    assert_eq!(strings.get(1).map(String::as_str), Some("b"));
    assert_eq!(strings.get(2).map(String::as_str), Some("c"));
}

#[test]
fn string_array_finder_assignment() {
    use swc_common::{FileName, SourceMap, sync::Lrc};
    use swc_ecma_parser::{EsSyntax, Parser, StringInput, Syntax};
    use swc_ecma_visit::VisitMutWith as _;

    let cm: Lrc<SourceMap> = Lrc::default();
    let fm = cm.new_source_file(
        FileName::Custom("test.js".into()).into(),
        r#"var arr; noop(arr = ["a", "b", "c", "d", "e"]); function noop() {}"#,
    );

    let mut parser = Parser::new(
        Syntax::Es(EsSyntax::default()),
        StringInput::from(&*fm),
        None,
    );

    let mut script = parser.parse_script().unwrap();
    let mut finder = StringArrayFinder::new();
    script.visit_mut_with(&mut finder);

    assert!(finder.arrays.contains_key("arr"));
}

#[test]
fn decoder_offset_extraction() {
    // Test the offset extraction logic
    use swc_common::{FileName, SourceMap, sync::Lrc};
    use swc_ecma_parser::{EsSyntax, Parser, StringInput, Syntax};

    let cm: Lrc<SourceMap> = Lrc::default();
    let fm = cm.new_source_file(FileName::Custom("test.js".into()).into(), r"x - 123");

    let mut parser = Parser::new(
        Syntax::Es(EsSyntax::default()),
        StringInput::from(&*fm),
        None,
    );

    let expr = parser.parse_expr().unwrap();
    let offset = DecoderFunctionFinder::extract_offset(&expr);
    let expected: i32 = -123;
    assert_eq!(offset, Some(expected));
}

#[test]
fn decoder_offset_extraction_add() {
    use swc_common::{FileName, SourceMap, sync::Lrc};
    use swc_ecma_parser::{EsSyntax, Parser, StringInput, Syntax};

    let cm: Lrc<SourceMap> = Lrc::default();
    let fm = cm.new_source_file(FileName::Custom("test.js".into()).into(), r"x + 456");

    let mut parser = Parser::new(
        Syntax::Es(EsSyntax::default()),
        StringInput::from(&*fm),
        None,
    );

    let expr = parser.parse_expr().unwrap();
    let offset = DecoderFunctionFinder::extract_offset(&expr);
    let expected: i32 = 456;
    assert_eq!(offset, Some(expected));
}

#[test]
fn decoder_function_finder_extracts_self_redef_offset() {
    use std::collections::HashMap;

    use swc_common::{FileName, Globals, SourceMap, sync::Lrc};
    use swc_ecma_parser::{EsSyntax, Parser, StringInput, Syntax};
    use swc_ecma_visit::VisitMutWith as _;

    let code = r"
function _0xbba6(_0x9779e0,_0x3727db){
  const _0x2a129a=_0x9fd6();
  return _0xbba6=function(_0x5e4e8c,_0x244e6b){
_0x5e4e8c=_0x5e4e8c-(-0x2315*0x1+0x1938+0xa62);
let _0x172341=_0x2a129a[_0x5e4e8c];
return _0x172341;
  },_0xbba6(_0x9779e0,_0x3727db);
}
";

    let cm: Lrc<SourceMap> = Lrc::default();
    let fm = cm.new_source_file(FileName::Custom("test.js".into()).into(), code);

    let mut parser = Parser::new(
        Syntax::Es(EsSyntax::default()),
        StringInput::from(&*fm),
        None,
    );

    let script = parser.parse_script().unwrap();
    let mut program = Program::Script(script);

    let string_arrays = vec![StringArray {
        identifier: "_0x9fd6".to_owned(),
        array_type: StringArrayType::Function,
        strings: Vec::new(),
    }];

    let helper_decoders = HashMap::new();
    let mut finder = DecoderFunctionFinder::new(&string_arrays, &helper_decoders);

    GLOBALS.set(&Globals::default(), || {
        program.visit_mut_with(&mut finder);
    });

    assert_eq!(finder.decoders.len(), 1);
    let decoder = finder.decoders.first().expect("decoder");
    assert_eq!(decoder.identifier, "_0xbba6");
    assert_eq!(decoder.string_array_identifier, "_0x9fd6");
    let expected: i32 = -133;
    assert_eq!(decoder.offset, expected);
}

#[test]
fn stringdecoder_decodes_multi_level_wrapper_calls() {
    let deob = Deobfuscator::new();
    let code = r#"
function _0xarr() {
  const _0x2a129a = ["foo", "bar", "baz"];
  _0xarr = function() { return _0x2a129a; };
  return _0xarr();
}
function _0xbba6(_0x9779e0, _0x3727db) {
  const _0x2a129a = _0xarr();
  _0xbba6 = function(_0x5e4e8c, _0x244e6b) {
_0x5e4e8c = _0x5e4e8c - (0);
let _0x172341 = _0x2a129a[_0x5e4e8c];
return _0x172341;
  };
  return _0xbba6(_0x9779e0, _0x3727db);
}
function _0x40e6ec(_0x16b97b, _0x25372d, _0x291b16, _0x20a4c7, _0x29edc4) {
  return _0xbba6(_0x25372d - -0, _0x16b97b);
}
function _0x4f33c7(_0x6fbb93, _0x51a83f, _0x4c30c6, _0xf312d4, _0x39d538) {
  return _0x40e6ec(_0x6fbb93, _0x4c30c6 - 1, 0, 0, 0);
}
const x = _0x4f33c7(0, 0, 1, 0, 0);
"#;

    let options = DeobfuscateOptions {
        custom_transformers: Some(vec![Arc::new(StringDecoder::new())]),
        ..DeobfuscateOptions::default()
    };

    let result = deob.deobfuscate_source(code, Some(options)).unwrap();
    assert!(result.contains("\"foo\""));
    assert!(result.contains("x = \"foo\""));
    assert!(!result.contains("_0x4f33c7(0"));
}
