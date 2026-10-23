//! `ERB` compiled the way MRI's `ERB::Compiler` compiles it: every trim mode,
//! the `%` line prefix, `%%>` inside a tag, `#{…}` in template text, and the
//! exact `#src` MRI answers. Every expectation below was produced by
//! `ruby -rerb` (ruby 4.0.7, erb 6.0.7) and frozen here.

use rubylang::eval_to_string as ev;
use std::process::Command;

fn eq(src: &str, expected: &str) {
    match ev(src) {
        Ok(got) => assert_eq!(got, expected, "for source: {src}"),
        Err(e) => panic!("eval error for `{src}`: {e}"),
    }
}

/// `ERB.new(template, trim_mode: mode).result` for a template written as Ruby
/// source, asserting its `inspect`.
fn renders(template: &str, mode: &str, expected: &str) {
    eq(
        &format!("ERB.new({template}, trim_mode: {mode}).result(binding)"),
        expected,
    );
}

const T1: &str = r#""a\n<% x = 1 %>\nb <%= x %> c\n<%# note %>\n<%% lit %%>\nend""#;
const T2: &str = r#""<% [1,2].each do |i| %>\n  item <%= i %>\n<% end %>\ntail\n""#;
const T3: &str =
    r#""% x = 5\nplain\n%% not code\nv=<%= x %>\n% if x > 1\nbig\n% end\nmid % not\n""#;
const T4: &str = r#""  <%- y = 2 -%>\nfoo  <%- y -%>  bar\n<%= y -%>\nlast <%- 1 -%>""#;
const T6: &str = r#""<% a = 1 %>\r\nline\r\n<%= a %>\r\n""#;
const T7: &str = r#""<%\n  z = 3\n%>\nv<%= z %>\n<%#\n multi\n%>\nq""#;

#[test]
fn no_trim_keeps_every_newline() {
    renders(T1, "nil", r#""a\n\nb 1 c\n\n<% lit %%>\nend""#);
    renders(T1, "0", r#""a\n\nb 1 c\n\n<% lit %%>\nend""#);
    renders(T2, "nil", r#""\n  item 1\n\n  item 2\n\ntail\n""#);
    renders(T6, "nil", r#""\r\nline\r\n1\r\n""#);
    renders(T7, "nil", r#""\nv3\n\nq""#);
    // A `%` line is ordinary text outside the `%` mode: its `x` is never defined.
    eq(
        &format!("begin; ERB.new({T3}).result(binding); rescue NameError => e; e.message; end"),
        r#""undefined local variable or method 'x' for main""#,
    );
}

#[test]
fn gt_mode_drops_the_newline_after_every_closing_tag() {
    renders(T1, "\">\"", r#""a\nb 1 c\n<% lit %%>\nend""#);
    renders(T1, "1", r#""a\nb 1 c\n<% lit %%>\nend""#);
    renders(T2, "\">\"", r#""  item 1  item 2tail\n""#);
    renders(T2, "1", r#""  item 1  item 2tail\n""#);
    renders(r#""<%= 1 %>\n<%= 2 %>""#, "\">\"", r#""12""#);
    renders(T6, "\">\"", r#""line\r\n1""#);
    renders(T7, "\">\"", r#""v3q""#);
}

#[test]
fn ltgt_mode_drops_it_only_when_the_line_began_with_a_tag() {
    renders(T1, "\"<>\"", r#""a\nb 1 c\n<% lit %%>\nend""#);
    renders(T2, "\"<>\"", r#""  item 1\n  item 2\ntail\n""#);
    renders(T2, "2", r#""  item 1\n  item 2\ntail\n""#);
    renders(
        r#""  <%= 1 %>\n<%= 2 %>\nz <%= 3 %>\n""#,
        "\"<>\"",
        r#""  1\n2z 3\n""#,
    );
    renders(T6, "\"<>\"", r#""line\r\n1""#);
    renders(T7, "\"<>\"", r#""\nv3\n\nq""#);
    renders(r#""x\n""#, "2", r#""x\n""#);
}

#[test]
fn percent_lines_are_code_and_combine_with_the_other_modes() {
    renders(T3, "\"%\"", r#""plain\n% not code\nv=5\nbig\nmid % not\n""#);
    renders(
        T3,
        "\"%-\"",
        r#""plain\n% not code\nv=5\nbig\nmid % not\n""#,
    );
    renders(
        T3,
        "\"%<>\"",
        r#""plain\n% not code\nv=5\nbig\nmid % not\n""#,
    );
    renders(T3, "\"%>\"", r#""plain\n% not code\nv=5big\nmid % not\n""#);
    renders(T2, "\"%<>\"", r#""  item 1\n  item 2\ntail\n""#);
    renders(T2, "\"%>\"", r#""  item 1  item 2tail\n""#);
    renders(r#""t\n% if true\nyes\n% end\n""#, "\"%>\"", r#""t\nyes\n""#);
    renders(r#""%""#, "\"%\"", r#""""#);
    renders(r#""%%""#, "\"%\"", r#""%""#);
}

#[test]
fn dash_mode_trims_blanks_only_from_a_line_start() {
    // `foo  <%- y -%>  bar`: the blanks before a mid-line `<%-` are kept, and the
    // `-%>` at the very end of the template leaves `last ` intact.
    renders(T4, "\"-\"", r#""foo    bar\n2last ""#);
    renders(T4, "\"%-\"", r#""foo    bar\n2last ""#);
    renders(T4, "\"->\"", r#""foo    bar\n2last ""#);
    renders(
        r#""<% if true -%>\n  in\n<%- end -%>\nafter""#,
        "\"-\"",
        r#""  in\nafter""#,
    );
}

#[test]
fn template_text_is_dumped_so_interpolation_syntax_stays_literal() {
    renders(
        r##"'a #{1+1} b <%= "#{2+2}" %>'"##,
        "nil",
        r#""a \#{1+1} b 4""#,
    );
    renders(r#""a \\ \"q\" \t x\n""#, "nil", r#""a \\ \"q\" \t x\n""#);
}

#[test]
fn escaped_tags_and_odd_tag_bodies() {
    renders(
        r#""x <%= '%%>' %> y <%% z %%> w""#,
        "nil",
        r#""x %> y <% z %%> w""#,
    );
    // A Ruby comment inside `<% %>` comments out the rest of the generated line.
    renders(r#""a<% # c %>b""#, "nil", r#""a""#);
    renders(r#""unterminated <% x""#, "nil", r#""unterminated  x""#);
}

#[test]
fn src_is_mri_s_byte_for_byte() {
    eq(
        &format!("ERB.new({T1}).src"),
        r##""#coding:UTF-8\n_erbout = +''; _erbout.<< \"a\\n\".freeze\n;  x = 1 ; _erbout.<< \"\\nb \".freeze\n; _erbout.<<(( x ).to_s); _erbout.<< \" c\\n\".freeze\n; ; _erbout.<< \"\\n\".freeze\n; _erbout.<< \"<% lit %%>\\nend\".freeze\n; _erbout""##,
    );
    eq(
        &format!("ERB.new({T3}, trim_mode: \"%\").src"),
        r##""#coding:UTF-8\n_erbout = +'';  x = 5\n_erbout.<< \"plain\\n\".freeze\n; _erbout.<< \"% not code\\n\".freeze\n; _erbout.<< \"v=\".freeze; _erbout.<<(( x ).to_s); _erbout.<< \"\\n\".freeze\n;  if x > 1\n_erbout.<< \"big\\n\".freeze\n;  end\n_erbout.<< \"mid % not\\n\".freeze\n; _erbout""##,
    );
    eq(
        &format!("ERB.new({T4}, trim_mode: \"-\").src"),
        r##""#coding:UTF-8\n_erbout = +'';  y = 2 \n_erbout.<< \"foo  \".freeze;  y ; _erbout.<< \"  bar\\n\".freeze\n; _erbout.<<(( y ).to_s)\n_erbout.<< \"last \".freeze;  1 \n_erbout""##,
    );
    eq(
        &format!("ERB.new({T2}, trim_mode: \"<>\").src"),
        r##""#coding:UTF-8\n_erbout = +'';  [1,2].each do |i| \n_erbout.<< \"  item \".freeze; _erbout.<<(( i ).to_s); _erbout.<< \"\\n\".freeze\n;  end \n_erbout.<< \"tail\\n\".freeze\n; _erbout""##,
    );
    eq(
        r#"ERB.new("<%# frozen_string_literal: true %>\nx").src"#,
        r##""#coding:UTF-8\n#frozen-string-literal:true\n_erbout = +''; ; _erbout.<< \"\\nx\".freeze\n; _erbout""##,
    );
    eq(
        r#"ERB.new("<%# -*- coding: us-ascii -*- %>x").src"#,
        r##""#coding:US-ASCII\n_erbout = +''; ; _erbout.<< \"x\".freeze; _erbout""##,
    );
}

#[test]
fn an_unknown_trim_mode_warns_and_compiles_plainly() {
    let out = Command::new(env!("CARGO_BIN_EXE_ruby"))
        .args([
            "-e",
            r#"print ERB.new("a<%= 1 %>\n", trim_mode: "x").result"#,
        ])
        .output()
        .expect("spawn ruby");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "a1\n");
    let err = String::from_utf8_lossy(&out.stderr);
    // erb warns with `uplevel:`, so the line that called `ERB.new` leads.
    assert_eq!(
        err,
        "-e:1: warning: Invalid ERB trim mode: \"x\" (trim_mode: nil, 0, 1, 2, or String composed of '%' and/or '-', '>', '<>')\n"
    );
    assert!(out.status.success());
}

#[test]
fn the_positional_trim_mode_of_old_erb_is_an_arity_error() {
    // erb 6 dropped `ERB.new(str, safe_level, trim_mode, eoutvar)`.
    eq(
        r#"begin; ERB.new("x", nil, "-"); rescue ArgumentError => e; e.message; end"#,
        r#""wrong number of arguments (given 3, expected 1)""#,
    );
    eq(
        r#"begin; ERB.new("x", 1); rescue ArgumentError => e; e.message; end"#,
        r#""wrong number of arguments (given 2, expected 1)""#,
    );
}
