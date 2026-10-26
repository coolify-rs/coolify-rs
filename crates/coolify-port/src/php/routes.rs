//! Static extraction of Laravel route declarations.
//!
//! Handles the constructs upstream `routes/*.php` files use:
//! `Route::<verb>(uri, action)`, `Route::match([...], uri, action)`,
//! `Route::any`, chained `->middleware(..)`, `->withoutMiddleware(..)`,
//! `Route::group([...], fn)` and `Route::prefix(..)->middleware(..)->group(fn)`.
//!
//! Correctness is checked against `php artisan route:list --json` output
//! captured from the reference image (see tests/route_parser_reference.rs).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::lexer::{Tok, Token, tokenize, use_imports};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct RouteDecl {
    /// Upper-case verbs without HEAD (Laravel adds HEAD to every GET).
    pub methods: Vec<String>,
    /// URI without leading slash, including group prefixes, e.g. `api/v1/projects`.
    pub uri: String,
    /// `Fully\Qualified\Controller@method`, or `Closure`.
    pub action: String,
    /// Middleware in application order, aliases resolved when an alias map is given.
    pub middleware: Vec<String>,
    /// Line of the declaration in the source file.
    pub line: u32,
}

impl RouteDecl {
    /// Stable identity used for inventory diffs: `METHODS uri`.
    pub fn key(&self) -> String {
        format!("{} {}", self.methods.join("|"), self.uri)
    }

    /// The `api.ability:<x>` requirements, in order (one entry per middleware).
    pub fn abilities(&self) -> Vec<String> {
        self.middleware
            .iter()
            .filter_map(|m| {
                m.strip_prefix("api.ability:")
                    .or_else(|| m.strip_prefix("App\\Http\\Middleware\\ApiAbility:"))
            })
            .map(str::to_string)
            .collect()
    }
}

#[derive(Debug, Clone, Default)]
pub struct ParseOptions {
    /// Prefix applied to every route (RouteServiceProvider), e.g. `api`.
    pub base_prefix: String,
    /// Middleware applied to every route (the route file's group), e.g. `api`.
    pub base_middleware: Vec<String>,
    /// Middleware alias -> class (from `app/Http/Kernel.php`).
    pub aliases: BTreeMap<String, String>,
}

#[derive(Debug, Clone)]
enum Value {
    Str(String),
    ClassConst(String),
    Array(Vec<(Option<String>, Value)>),
    Closure { body_start: usize, body_end: usize },
    Other,
}

#[derive(Debug, Clone, Default)]
struct Ctx {
    prefix: String,
    middleware: Vec<String>,
}

struct Parser<'a> {
    toks: &'a [Token],
    imports: BTreeMap<String, String>,
    opts: &'a ParseOptions,
    out: Vec<RouteDecl>,
}

const VERBS: &[&str] = &["get", "post", "put", "patch", "delete", "options"];
const ANY_VERBS: &[&str] = &["GET", "POST", "PUT", "PATCH", "DELETE", "OPTIONS"];

pub fn parse_routes(src: &str, opts: &ParseOptions) -> Vec<RouteDecl> {
    let toks = tokenize(src);
    let imports = use_imports(&toks);
    let mut p = Parser {
        toks: &toks,
        imports,
        opts,
        out: Vec::new(),
    };
    let ctx = Ctx {
        prefix: opts.base_prefix.trim_matches('/').to_string(),
        middleware: opts.base_middleware.clone(),
    };
    p.block(0, toks.len(), &ctx);
    p.out
}

/// Parses `protected $middlewareAliases = [...]` from a Laravel HTTP kernel.
pub fn parse_middleware_aliases(kernel_src: &str) -> BTreeMap<String, String> {
    let toks = tokenize(kernel_src);
    let imports = use_imports(&toks);
    let mut map = BTreeMap::new();
    let Some(start) = toks.iter().position(
        |t| matches!(&t.tok, Tok::Var(v) if v == "middlewareAliases" || v == "routeMiddleware"),
    ) else {
        return map;
    };
    let mut i = start;
    while i + 4 < toks.len() && toks[i].tok != Tok::Punct(";") {
        if let (Tok::Str(alias), Tok::Punct("=>"), Tok::Ident(class), Tok::Punct("::")) = (
            &toks[i].tok,
            &toks[i + 1].tok,
            &toks[i + 2].tok,
            &toks[i + 3].tok,
        ) {
            let fq = imports.get(class).cloned().unwrap_or_else(|| class.clone());
            map.insert(alias.clone(), fq);
        }
        i += 1;
    }
    map
}

impl Parser<'_> {
    fn block(&mut self, mut i: usize, end: usize, ctx: &Ctx) {
        while i < end {
            if self.toks[i].tok == Tok::Ident("Route".into())
                && self.toks.get(i + 1).map(|t| &t.tok) == Some(&Tok::Punct("::"))
            {
                i = self.chain(i + 2, end, ctx);
            } else {
                i += 1;
            }
        }
    }

    /// Parses `name(args)->name(args)...` starting after `Route::`.
    fn chain(&mut self, mut i: usize, end: usize, ctx: &Ctx) -> usize {
        let line = self.toks[i.saturating_sub(2)].line;
        let mut calls: Vec<(String, Vec<Value>)> = Vec::new();
        loop {
            let Some(Tok::Ident(name)) = self.toks.get(i).map(|t| &t.tok) else {
                break;
            };
            let name = name.clone();
            if self.toks.get(i + 1).map(|t| &t.tok) != Some(&Tok::Punct("(")) {
                break;
            }
            let (args, next) = self.args(i + 2, end);
            calls.push((name, args));
            i = next;
            if self.toks.get(i).map(|t| &t.tok) == Some(&Tok::Punct("->")) {
                i += 1;
            } else {
                break;
            }
        }
        self.interpret(calls, ctx, line);
        i
    }

    /// Parses a comma-separated argument list after `(`; returns index after `)`.
    fn args(&self, mut i: usize, end: usize) -> (Vec<Value>, usize) {
        let mut args = Vec::new();
        while i < end {
            match &self.toks[i].tok {
                Tok::Punct(")") => return (args, i + 1),
                Tok::Punct(",") => i += 1,
                _ => {
                    let (v, next) = self.value(i, end);
                    args.push(v);
                    i = next;
                }
            }
        }
        (args, end)
    }

    fn value(&self, i: usize, end: usize) -> (Value, usize) {
        match &self.toks[i].tok {
            Tok::Str(s) => (Value::Str(s.clone()), i + 1),
            Tok::Ident(id)
                if self.toks.get(i + 1).map(|t| &t.tok) == Some(&Tok::Punct("::"))
                    && self.toks.get(i + 2).map(|t| &t.tok)
                        == Some(&Tok::Ident("class".into())) =>
            {
                let fq = self.imports.get(id).cloned().unwrap_or_else(|| id.clone());
                (Value::ClassConst(fq), i + 3)
            }
            Tok::Ident(id) if id == "function" || id == "fn" || id == "static" => {
                // Find the body braces (skip params and `use (...)`).
                let mut j = i;
                while j < end && self.toks[j].tok != Tok::Punct("{") {
                    j += 1;
                }
                let close = self.matching(j, end);
                (
                    Value::Closure {
                        body_start: j + 1,
                        body_end: close,
                    },
                    close + 1,
                )
            }
            Tok::Punct("[") => {
                let mut items = Vec::new();
                let mut j = i + 1;
                while j < end && self.toks[j].tok != Tok::Punct("]") {
                    if self.toks[j].tok == Tok::Punct(",") {
                        j += 1;
                        continue;
                    }
                    let (v, next) = self.value(j, end);
                    if self.toks.get(next).map(|t| &t.tok) == Some(&Tok::Punct("=>")) {
                        let key = match v {
                            Value::Str(s) => Some(s),
                            _ => None,
                        };
                        let (val, after) = self.value(next + 1, end);
                        items.push((key, val));
                        j = after;
                    } else {
                        items.push((None, v));
                        j = next;
                    }
                }
                (Value::Array(items), j + 1)
            }
            _ => {
                // Skip an unknown expression up to the next `,`/`)`/`]` at depth 0.
                let mut j = i;
                let mut depth = 0i32;
                while j < end {
                    match &self.toks[j].tok {
                        Tok::Punct("(") | Tok::Punct("[") | Tok::Punct("{") => depth += 1,
                        Tok::Punct(")") | Tok::Punct("]") | Tok::Punct("}") if depth == 0 => break,
                        Tok::Punct(")") | Tok::Punct("]") | Tok::Punct("}") => depth -= 1,
                        Tok::Punct(",") if depth == 0 => break,
                        _ => {}
                    }
                    j += 1;
                }
                (Value::Other, j.max(i + 1))
            }
        }
    }

    fn matching(&self, open: usize, end: usize) -> usize {
        let mut depth = 0i32;
        let mut j = open;
        while j < end {
            match self.toks[j].tok {
                Tok::Punct("{") => depth += 1,
                Tok::Punct("}") => {
                    depth -= 1;
                    if depth == 0 {
                        return j;
                    }
                }
                _ => {}
            }
            j += 1;
        }
        end
    }

    fn interpret(&mut self, calls: Vec<(String, Vec<Value>)>, ctx: &Ctx, line: u32) {
        let mut group_ctx = ctx.clone();
        let mut route: Option<(Vec<String>, String, String)> = None;
        let mut route_mw: Vec<String> = Vec::new();
        let mut without: Vec<String> = Vec::new();
        let mut group_body: Option<(usize, usize)> = None;

        for (name, args) in &calls {
            match name.as_str() {
                v if VERBS.contains(&v) => {
                    route = self.route_parts(vec![v.to_uppercase()], args.first(), args.get(1));
                }
                "any" => {
                    let verbs = ANY_VERBS.iter().map(|s| s.to_string()).collect();
                    route = self.route_parts(verbs, args.first(), args.get(1));
                }
                "match" => {
                    let verbs = match args.first() {
                        Some(Value::Array(items)) => items
                            .iter()
                            .filter_map(|(_, v)| match v {
                                Value::Str(s) => Some(s.to_uppercase()),
                                _ => None,
                            })
                            .filter(|v| v != "HEAD")
                            .collect(),
                        Some(Value::Str(s)) => vec![s.to_uppercase()],
                        _ => Vec::new(),
                    };
                    route = self.route_parts(verbs, args.get(1), args.get(2));
                }
                "prefix" => {
                    if let Some(Value::Str(p)) = args.first() {
                        group_ctx.prefix = join_uri(&group_ctx.prefix, p);
                    }
                }
                "middleware" => {
                    let mw = flatten_strings(args);
                    if route.is_some() {
                        route_mw.extend(mw);
                    } else {
                        group_ctx.middleware.extend(mw);
                    }
                }
                "withoutMiddleware" => without.extend(flatten_strings(args)),
                "group" => {
                    for arg in args {
                        match arg {
                            Value::Array(items) => {
                                for (k, v) in items {
                                    match (k.as_deref(), v) {
                                        (Some("prefix"), Value::Str(p)) => {
                                            group_ctx.prefix = join_uri(&group_ctx.prefix, p);
                                        }
                                        (Some("middleware"), v) => {
                                            group_ctx
                                                .middleware
                                                .extend(flatten_strings(std::slice::from_ref(v)));
                                        }
                                        _ => {}
                                    }
                                }
                            }
                            Value::Closure {
                                body_start,
                                body_end,
                            } => group_body = Some((*body_start, *body_end)),
                            _ => {}
                        }
                    }
                }
                _ => {}
            }
        }

        if let Some((start, end)) = group_body {
            self.block(start, end, &group_ctx);
            return;
        }
        if let Some((methods, uri, action)) = route {
            let mut middleware: Vec<String> =
                ctx.middleware.iter().cloned().chain(route_mw).collect();
            middleware = middleware
                .into_iter()
                .map(|m| self.resolve_alias(&m))
                .collect();
            let without: Vec<String> = without.iter().map(|m| self.resolve_alias(m)).collect();
            middleware.retain(|m| !without.contains(m));
            self.out.push(RouteDecl {
                methods,
                uri: join_uri(&ctx.prefix, &uri),
                action,
                middleware,
                line,
            });
        }
    }

    fn route_parts(
        &self,
        methods: Vec<String>,
        uri: Option<&Value>,
        action: Option<&Value>,
    ) -> Option<(Vec<String>, String, String)> {
        let Value::Str(uri) = uri? else { return None };
        let action = match action {
            Some(Value::Array(items)) => match (items.first(), items.get(1)) {
                (Some((_, Value::ClassConst(c))), Some((_, Value::Str(m)))) => format!("{c}@{m}"),
                _ => "Unknown".into(),
            },
            Some(Value::ClassConst(c)) => c.clone(),
            Some(Value::Str(s)) => s.clone(),
            Some(Value::Closure { .. }) => "Closure".into(),
            _ => "Unknown".into(),
        };
        Some((methods, uri.clone(), action))
    }

    fn resolve_alias(&self, mw: &str) -> String {
        if self.opts.aliases.is_empty() {
            return mw.to_string();
        }
        let (name, params) = match mw.split_once(':') {
            Some((n, p)) => (n, Some(p)),
            None => (mw, None),
        };
        let resolved = self
            .opts
            .aliases
            .get(name)
            .cloned()
            .unwrap_or_else(|| name.to_string());
        match params {
            Some(p) => format!("{resolved}:{p}"),
            None => resolved,
        }
    }
}

fn flatten_strings(args: &[Value]) -> Vec<String> {
    let mut out = Vec::new();
    for a in args {
        match a {
            Value::Str(s) => out.push(s.clone()),
            Value::ClassConst(c) => out.push(c.clone()),
            Value::Array(items) => {
                out.extend(flatten_strings(
                    &items.iter().map(|(_, v)| v.clone()).collect::<Vec<_>>(),
                ));
            }
            _ => {}
        }
    }
    out
}

fn join_uri(prefix: &str, uri: &str) -> String {
    let p = prefix.trim_matches('/');
    let u = uri.trim_matches('/');
    match (p.is_empty(), u.is_empty()) {
        (true, _) => u.to_string(),
        (false, true) => p.to_string(),
        (false, false) => format!("{p}/{u}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_groups_and_middleware() {
        let src = r#"<?php
use App\Http\Controllers\Api\TeamController;
use App\Http\Middleware\ApiAllowed;
Route::get('/health', [TeamController::class, 'h']);
Route::group(['middleware' => ['auth:sanctum', ApiAllowed::class], 'prefix' => 'v1'], function () {
    Route::get('/teams', [TeamController::class, 'teams'])->middleware(['api.ability:read']);
    Route::prefix('x')->group(function () {
        Route::match(['get', 'post'], '/y/{id}', [TeamController::class, 'y'])->middleware('api.ability:write');
    });
});
Route::any('/{any}', function () { return 1; })->where('any', '.*');
"#;
        let opts = ParseOptions {
            base_prefix: "api".into(),
            base_middleware: vec!["api".into()],
            aliases: BTreeMap::new(),
        };
        let r = parse_routes(src, &opts);
        assert_eq!(r.len(), 4);
        assert_eq!(r[0].uri, "api/health");
        assert_eq!(r[0].action, "App\\Http\\Controllers\\Api\\TeamController@h");
        assert_eq!(r[1].uri, "api/v1/teams");
        assert_eq!(
            r[1].middleware,
            vec![
                "api",
                "auth:sanctum",
                "App\\Http\\Middleware\\ApiAllowed",
                "api.ability:read"
            ]
        );
        assert_eq!(r[1].abilities(), vec!["read"]);
        assert_eq!(r[2].uri, "api/v1/x/y/{id}");
        assert_eq!(r[2].methods, vec!["GET", "POST"]);
        assert_eq!(r[3].action, "Closure");
        assert_eq!(r[3].methods.len(), 6);
        assert_eq!(r[1].line, 6);
    }
}
