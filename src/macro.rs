#![crate_name="json_macros"]
#![crate_type="dylib"]
#![feature(phase, plugin_registrar, macro_rules, quote)]

#[phase(plugin, link)] extern crate log;
extern crate rustc;
extern crate syntax;
extern crate serialize;

use syntax::ast::TokenTree;
use syntax::codemap::Span;
use syntax::ptr::P;

use syntax::ast;
use syntax::ext::base::{ExtCtxt, MacResult, MacExpr, DummyResult};
use syntax::parse::token;
use syntax::print::pprust;
use rustc::plugin::Registry;

type PExpr = P<ast::Expr>;

#[plugin_registrar]
pub fn plugin_registrar(reg: &mut Registry) {
    reg.register_macro("json", expand);
}

fn expand<'cx>(cx: &'cx mut ExtCtxt, sp: Span,
               tts: &[TokenTree]) -> Box<MacResult + 'cx> {
    debug!("JSON token tree {}", tts);

    let tt = match tts.get(0) {
        Some(tt) => tt,
        None => {
            cx.span_err(sp, "expected JSON literal");
            return DummyResult::expr(sp);
        }
    };

    let expr = match tt_to_expr(cx, tt) {
        Some(e) => e,
        None => return DummyResult::expr(sp)
    };
    MacExpr::new(expr)
}

fn tt_to_expr(cx: &ExtCtxt, tt: &TokenTree) -> Option<PExpr> {
    use syntax::ext::build::AstBuilder;

    match *tt {
        ast::TtToken(sp, ref tok) => token_to_expr(cx, sp, tok),
        ast::TtDelimited(sp, ref d) => {
            let ref toks = d.tts;
            match d.delim {
                // array
                token::Bracket => {
                    let exprs = match parse_array(cx, sp, toks.as_slice()) {
                        Some(e) => e,
                        None => return None
                    };
                    let exprs = cx.expr_vec(sp, exprs);
                    Some(quote_expr!(cx, {
                        {
                            use std::slice::BoxedSlicePrelude;
                            let xs: ::std::boxed::Box<[_]> = box $exprs;
                            ::serialize::json::List(xs.into_vec())
                        }
                    }))
                }
                // object
                token::Brace => {
                    let items = match parse_object(cx, sp, toks.as_slice()) {
                        Some(i) => i,
                        None => return None
                    };
                    let ob = quote_expr!(cx, _ob);
                    let mut stmts = vec![];
                    for &(ref key, ref value) in items.iter() {
                        stmts.push(quote_stmt!(cx, $ob.insert($key, $value)));
                    }

                    Some(quote_expr!(cx, {
                        {
                            let mut $ob = ::std::collections::TreeMap::new();
                            $stmts;
                            ::serialize::json::Object($ob)
                        }
                    }))
                }
                token::Paren => {
                    let mut parser = cx.new_parser_from_tts(toks.as_slice());
                    let expr = parser.parse_expr();

                    Some(quote_expr!(cx, {
                        use serialize::json::ToJson;
                        ($expr).to_json()
                    }))
                }
            }
        }
        ast::TtSequence(sp, _) => {
            cx.span_err(sp, "`json!` unexpected TtSequence");
            None
        }
    }
}

fn parse_array(cx: &ExtCtxt, sp: Span, tts: &[TokenTree]) -> Option<Vec<PExpr>> {
    let mut exprs = Vec::with_capacity(tts.len() / 2);
    for (i, tt) in tts.iter().enumerate() {
        if i % 2 == 1 {
            match tt {
                &ast::TtToken(_, token::Comma) => {
                    continue;
                }
                _ => {
                    expected_but_found(cx, sp, "`,`", tt);
                    return None;
                }
            }
        }
        let expr = tt_to_expr(cx, tt);
        let expr = match expr {
            Some(e) => e,
            None => return None,
        };
        exprs.push(expr);
    }
    Some(exprs)
}

fn parse_object(cx: &ExtCtxt, sp: Span, tts: &[TokenTree]) -> Option<Vec<(PExpr, PExpr)>> {
    use syntax::ast::TtToken;
    use syntax::parse::token as tok;

    macro_rules! comma {
        () => {
            ::syntax::ast::TtToken(_, ::syntax::parse::token::Comma)
        }
    }

    macro_rules! colon {
        () => {
            ::syntax::ast::TtToken(_, ::syntax::parse::token::Colon)
        };
        ($sp:ident) => {
            ::syntax::ast::TtToken($sp, ::syntax::parse::token::Colon)
        }
    }

    let mut items = Vec::with_capacity(tts.len() / 4);
    if tts.len() == 0 {
        return Some(items);
    }

    // horrible
    for entry in tts.chunks(4) {
        let item = match entry {
            // "foo": VALUE | "foo": VALUE,
            [TtToken(_, tok::LitStr(ref n)), colon!(), ref v] |
            [TtToken(_, tok::LitStr(ref n)), colon!(), ref v, comma!()] => {
                let k = n.as_str();
                let v = tt_to_expr(cx, v);
                if v.is_none() {
                    return None;
                }
                let k = quote_expr!(cx, $k.to_string());
                let v = quote_expr!(cx, $v);
                (k, v)
            }
            // "foo": VALUE X
            [TtToken(_, tok::LitStr(_)), colon!(), _, ref tt] => {
                expected_but_found(cx, sp, "`,`", tt);
                return None;
            }
            [TtToken(_, tok::LitStr(_)), colon!(sp)] => {
                cx.span_err(sp, "found `:` but no value afterwards");
                return None;
            }
            [TtToken(_, tok::LitStr(_)), ref tt, ..] => {
                expected_but_found(cx, sp, "`:`", tt);
                return None;
            }
            [TtToken(sp, tok::LitStr(_))] => {
                cx.span_err(sp, "found name but no colon-value afterwards");
                return None;
            }
            [ref tt, ..] => {
                expected_but_found(cx, sp, "string literal", tt);
                return None;
            }
            [] => unreachable!(), // chunks() never returns an empty slice
        };
        items.push(item);
    }

    Some(items)
}

fn token_to_expr(cx: &ExtCtxt, sp: Span, tok: &token::Token) -> Option<PExpr> {
    use syntax::print::pprust;

    match *tok {
        token::LitStr(ref n) => {
            let s = n.as_str();
            Some(quote_expr!(cx, {
                ::serialize::json::String($s.into_string())
            }))
        }
        // FIXME: handle suffixed literals (i.e. u64) correctly
        // FIXME: handle negative numbers
        token::LitInteger(_) => {
            let tt = ast::TtToken(sp, tok.clone());
            Some(quote_expr!(cx, {
                ::serialize::json::I64($tt as i64)
            }))
        }
        token::LitFloat(_) => {
            let tt = ast::TtToken(sp, tok.clone());
            Some(quote_expr!(cx, {
                ::serialize::json::F64($tt)
            }))
        }
        token::Ident(ref id, token::Plain) if id.as_str() == "null" => {
            Some(quote_expr!(cx, {
                ::serialize::json::Null
            }))
        }
        ref t @ token::Ident(..) if t.is_keyword(token::keywords::True) => {
            Some(quote_expr!(cx, { ::serialize::json::Boolean(true) }))
        }
        ref t @ token::Ident(..) if t.is_keyword(token::keywords::False) => {
            Some(quote_expr!(cx, { ::serialize::json::Boolean(false) }))
        }
        _ => {
            let tt = ast::TtToken(sp, tok.clone());
            let s = pprust::tt_to_string(&tt);
            cx.span_err(sp, format!("unexpected `{}` in JSON", s).as_slice());
            None
        }
    }
}

fn best_span(sp: Span, tt: &TokenTree) -> Span {
    let sp = match *tt {
        ast::TtToken(tok_sp, _) => tok_sp,
        _ => sp // the span passed into the function!
    };
    sp
}

fn expected_but_found(cx: &ExtCtxt, sp: Span, expected: &str, found: &TokenTree) {
    let pp = pprust::tt_to_string(found);
    let err = format!("expected {} but found: `{}`", expected, pp);
    cx.span_err(best_span(sp, found), err.as_slice());
}
