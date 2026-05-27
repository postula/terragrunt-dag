//! Minimal HCL evaluator used to bind `values = { ... }` from
//! `terragrunt.stack.hcl` units.
//!
//! Wraps `hcl-rs`'s built-in evaluator and registers a small set of helper
//! functions sufficient to evaluate the expressions that real-world stack
//! files use to compute dependency paths. Functions are intentionally
//! conservative: anything we cannot evaluate raises an error so the caller
//! can warn-and-skip.

use hcl::Value;
use hcl::eval::{Context, Evaluate, FuncDef, ParamType};

/// Build an evaluation context with `values` and `local` declared and our
/// helper functions registered.
pub fn build_context(values: &Value, locals: &Value) -> Context<'static> {
    let mut ctx = Context::new();
    ctx.declare_var("values", values.clone());
    ctx.declare_var("local", locals.clone());
    register_helpers(&mut ctx);
    register_terragrunt_stubs(&mut ctx);
    ctx
}

fn register_helpers(ctx: &mut Context<'_>) {
    ctx.declare_func(
        "merge",
        FuncDef::builder().variadic_param(ParamType::Object(Box::new(ParamType::Any))).build(merge_func),
    );

    ctx.declare_func(
        "format",
        FuncDef::builder().param(ParamType::String).variadic_param(ParamType::Any).build(format_func),
    );

    // `try(expr, fallback)` cannot replicate terragrunt's lazy semantics
    // (errors during evaluation propagate before the func runs), but it
    // resolves the common `try(value, default)` pattern when the first arg
    // is null.
    ctx.declare_func("try", FuncDef::builder().param(ParamType::Any).variadic_param(ParamType::Any).build(try_func));

    ctx.declare_func("tostring", FuncDef::builder().param(ParamType::Any).build(tostring_func));

    ctx.declare_func("coalesce", FuncDef::builder().variadic_param(ParamType::Any).build(coalesce_func));

    ctx.declare_func(
        "lookup",
        FuncDef::builder()
            .param(ParamType::Object(Box::new(ParamType::Any)))
            .param(ParamType::String)
            .param(ParamType::Any)
            .build(lookup_func),
    );
}

fn register_terragrunt_stubs(ctx: &mut Context<'_>) {
    // These stubs let stack files reference common terragrunt builtins
    // without aborting evaluation. Path resolution itself is handled by
    // `ResolveContext`; here we only need to return *something* sensible.
    ctx.declare_func("get_repo_root", FuncDef::builder().build(empty_string_func));
    ctx.declare_func("get_terragrunt_dir", FuncDef::builder().build(empty_string_func));
    ctx.declare_func(
        "find_in_parent_folders",
        FuncDef::builder().variadic_param(ParamType::String).build(empty_string_func),
    );
    ctx.declare_func(
        "get_env",
        FuncDef::builder().param(ParamType::String).variadic_param(ParamType::Any).build(get_env_func),
    );

    // Common HCL/Terraform builtins that frequently appear in real-world
    // stack files. They are not semantically modelled here; we only need
    // their evaluation to succeed so the surrounding expression can return.
    ctx.declare_func("jsonencode", FuncDef::builder().param(ParamType::Any).build(stub_string_func));
    ctx.declare_func("yamlencode", FuncDef::builder().param(ParamType::Any).build(stub_string_func));
    ctx.declare_func("jsondecode", FuncDef::builder().param(ParamType::String).build(stub_object_func));
    ctx.declare_func("yamldecode", FuncDef::builder().param(ParamType::String).build(stub_object_func));
    ctx.declare_func("file", FuncDef::builder().param(ParamType::String).build(stub_string_func));
    ctx.declare_func("fileexists", FuncDef::builder().param(ParamType::String).build(false_func));
    ctx.declare_func("trimspace", FuncDef::builder().param(ParamType::String).build(trimspace_func));
    ctx.declare_func("lower", FuncDef::builder().param(ParamType::String).build(lower_func));
    ctx.declare_func("upper", FuncDef::builder().param(ParamType::String).build(upper_func));
    ctx.declare_func(
        "replace",
        FuncDef::builder()
            .param(ParamType::String)
            .param(ParamType::String)
            .param(ParamType::String)
            .build(replace_func),
    );
    ctx.declare_func("split", FuncDef::builder().param(ParamType::String).param(ParamType::String).build(split_func));
    ctx.declare_func(
        "join",
        FuncDef::builder().param(ParamType::String).param(ParamType::Array(Box::new(ParamType::Any))).build(join_func),
    );
    ctx.declare_func(
        "concat",
        FuncDef::builder().variadic_param(ParamType::Array(Box::new(ParamType::Any))).build(concat_func),
    );
    ctx.declare_func("length", FuncDef::builder().param(ParamType::Any).build(length_func));
    ctx.declare_func("keys", FuncDef::builder().param(ParamType::Object(Box::new(ParamType::Any))).build(keys_func));
    ctx.declare_func(
        "values",
        FuncDef::builder().param(ParamType::Object(Box::new(ParamType::Any))).build(values_func),
    );
    ctx.declare_func(
        "contains",
        FuncDef::builder().param(ParamType::Array(Box::new(ParamType::Any))).param(ParamType::Any).build(contains_func),
    );
    ctx.declare_func(
        "regex_replace",
        FuncDef::builder()
            .param(ParamType::String)
            .param(ParamType::String)
            .param(ParamType::String)
            .build(regex_replace_func),
    );
    ctx.declare_func("abspath", FuncDef::builder().param(ParamType::String).build(passthrough_string_func));
    ctx.declare_func("basename", FuncDef::builder().param(ParamType::String).build(basename_func));
    ctx.declare_func("dirname", FuncDef::builder().param(ParamType::String).build(dirname_func));
    ctx.declare_func("pathexpand", FuncDef::builder().param(ParamType::String).build(passthrough_string_func));
}

fn stub_string_func(_args: hcl::eval::FuncArgs) -> Result<Value, String> {
    Ok(Value::String(String::new()))
}

fn stub_object_func(_args: hcl::eval::FuncArgs) -> Result<Value, String> {
    Ok(Value::Object(hcl::Map::new()))
}

fn false_func(_args: hcl::eval::FuncArgs) -> Result<Value, String> {
    Ok(Value::Bool(false))
}

fn trimspace_func(args: hcl::eval::FuncArgs) -> Result<Value, String> {
    let Some(Value::String(s)) = args.into_values().into_iter().next() else {
        return Err("trimspace: expected string".to_string());
    };
    Ok(Value::String(s.trim().to_string()))
}

fn lower_func(args: hcl::eval::FuncArgs) -> Result<Value, String> {
    let Some(Value::String(s)) = args.into_values().into_iter().next() else {
        return Err("lower: expected string".to_string());
    };
    Ok(Value::String(s.to_lowercase()))
}

fn upper_func(args: hcl::eval::FuncArgs) -> Result<Value, String> {
    let Some(Value::String(s)) = args.into_values().into_iter().next() else {
        return Err("upper: expected string".to_string());
    };
    Ok(Value::String(s.to_uppercase()))
}

fn replace_func(args: hcl::eval::FuncArgs) -> Result<Value, String> {
    let mut iter = args.into_values().into_iter();
    let Some(Value::String(s)) = iter.next() else {
        return Err("replace: expected string".to_string());
    };
    let Some(Value::String(from)) = iter.next() else {
        return Err("replace: expected string".to_string());
    };
    let Some(Value::String(to)) = iter.next() else {
        return Err("replace: expected string".to_string());
    };
    Ok(Value::String(s.replace(&from, &to)))
}

fn split_func(args: hcl::eval::FuncArgs) -> Result<Value, String> {
    let mut iter = args.into_values().into_iter();
    let Some(Value::String(sep)) = iter.next() else {
        return Err("split: expected separator string".to_string());
    };
    let Some(Value::String(s)) = iter.next() else {
        return Err("split: expected string".to_string());
    };
    Ok(Value::Array(s.split(&sep).map(|p| Value::String(p.to_string())).collect()))
}

fn join_func(args: hcl::eval::FuncArgs) -> Result<Value, String> {
    let mut iter = args.into_values().into_iter();
    let Some(Value::String(sep)) = iter.next() else {
        return Err("join: expected separator string".to_string());
    };
    let Some(Value::Array(items)) = iter.next() else {
        return Err("join: expected array".to_string());
    };
    let parts: Vec<String> = items.iter().map(value_to_display_string).collect();
    Ok(Value::String(parts.join(&sep)))
}

fn concat_func(args: hcl::eval::FuncArgs) -> Result<Value, String> {
    let mut out = Vec::new();
    for arg in args.into_values() {
        let Value::Array(items) = arg else {
            return Err("concat: expected arrays".to_string());
        };
        out.extend(items);
    }
    Ok(Value::Array(out))
}

fn length_func(args: hcl::eval::FuncArgs) -> Result<Value, String> {
    let Some(v) = args.into_values().into_iter().next() else {
        return Err("length: missing argument".to_string());
    };
    let n = match v {
        Value::Array(a) => a.len(),
        Value::Object(o) => o.len(),
        Value::String(s) => s.chars().count(),
        _ => return Err("length: unsupported type".to_string()),
    };
    Ok(Value::Number(hcl::Number::from(n as u64)))
}

fn keys_func(args: hcl::eval::FuncArgs) -> Result<Value, String> {
    let Some(Value::Object(obj)) = args.into_values().into_iter().next() else {
        return Err("keys: expected object".to_string());
    };
    Ok(Value::Array(obj.keys().map(|k| Value::String(k.to_string())).collect()))
}

fn values_func(args: hcl::eval::FuncArgs) -> Result<Value, String> {
    let Some(Value::Object(obj)) = args.into_values().into_iter().next() else {
        return Err("values: expected object".to_string());
    };
    Ok(Value::Array(obj.into_values().collect()))
}

fn contains_func(args: hcl::eval::FuncArgs) -> Result<Value, String> {
    let mut iter = args.into_values().into_iter();
    let Some(Value::Array(items)) = iter.next() else {
        return Err("contains: expected array".to_string());
    };
    let needle = iter.next().unwrap_or(Value::Null);
    Ok(Value::Bool(items.contains(&needle)))
}

fn regex_replace_func(args: hcl::eval::FuncArgs) -> Result<Value, String> {
    // We don't ship a regex engine; fall back to plain string replacement.
    let mut iter = args.into_values().into_iter();
    let Some(Value::String(s)) = iter.next() else {
        return Err("regex_replace: expected string".to_string());
    };
    let Some(Value::String(pat)) = iter.next() else {
        return Err("regex_replace: expected pattern".to_string());
    };
    let Some(Value::String(repl)) = iter.next() else {
        return Err("regex_replace: expected replacement".to_string());
    };
    Ok(Value::String(s.replace(&pat, &repl)))
}

fn passthrough_string_func(args: hcl::eval::FuncArgs) -> Result<Value, String> {
    let Some(Value::String(s)) = args.into_values().into_iter().next() else {
        return Err("expected string".to_string());
    };
    Ok(Value::String(s))
}

fn basename_func(args: hcl::eval::FuncArgs) -> Result<Value, String> {
    let Some(Value::String(s)) = args.into_values().into_iter().next() else {
        return Err("basename: expected string".to_string());
    };
    let base = s.rsplit('/').next().unwrap_or("").to_string();
    Ok(Value::String(base))
}

fn dirname_func(args: hcl::eval::FuncArgs) -> Result<Value, String> {
    let Some(Value::String(s)) = args.into_values().into_iter().next() else {
        return Err("dirname: expected string".to_string());
    };
    match s.rfind('/') {
        Some(idx) => Ok(Value::String(s[..idx].to_string())),
        None => Ok(Value::String(String::new())),
    }
}

fn merge_func(args: hcl::eval::FuncArgs) -> Result<Value, String> {
    let mut out = hcl::Map::new();
    for arg in args.into_values() {
        let Value::Object(map) = arg else {
            return Err("merge: expected object arguments".to_string());
        };
        for (k, v) in map {
            out.insert(k, v);
        }
    }
    Ok(Value::Object(out))
}

fn format_func(args: hcl::eval::FuncArgs) -> Result<Value, String> {
    let values = args.into_values();
    let mut iter = values.into_iter();
    let fmt = match iter.next() {
        Some(Value::String(s)) => s,
        _ => return Err("format: first argument must be a string".to_string()),
    };

    let mut out = String::new();
    let mut chars = fmt.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        let spec = chars.next().ok_or_else(|| "format: trailing %".to_string())?;
        match spec {
            '%' => out.push('%'),
            's' | 'v' | 'd' => {
                let next = iter.next().ok_or_else(|| "format: too few arguments".to_string())?;
                out.push_str(&value_to_display_string(&next));
            }
            other => return Err(format!("format: unsupported verb %{}", other)),
        }
    }

    Ok(Value::String(out))
}

fn try_func(args: hcl::eval::FuncArgs) -> Result<Value, String> {
    for value in args.into_values() {
        if !matches!(value, Value::Null) {
            return Ok(value);
        }
    }
    Ok(Value::Null)
}

fn tostring_func(args: hcl::eval::FuncArgs) -> Result<Value, String> {
    let value = args.into_values().into_iter().next().ok_or_else(|| "tostring: missing argument".to_string())?;
    Ok(Value::String(value_to_display_string(&value)))
}

fn coalesce_func(args: hcl::eval::FuncArgs) -> Result<Value, String> {
    for value in args.into_values() {
        if !matches!(value, Value::Null) {
            return Ok(value);
        }
    }
    Ok(Value::Null)
}

fn lookup_func(args: hcl::eval::FuncArgs) -> Result<Value, String> {
    let mut iter = args.into_values().into_iter();
    let map = iter.next().ok_or_else(|| "lookup: missing map".to_string())?;
    let key = iter.next().ok_or_else(|| "lookup: missing key".to_string())?;
    let default = iter.next().unwrap_or(Value::Null);

    let Value::Object(obj) = map else {
        return Err("lookup: first argument must be an object".to_string());
    };
    let Value::String(key) = key else {
        return Err("lookup: key must be a string".to_string());
    };

    Ok(obj.get(&key).cloned().unwrap_or(default))
}

fn empty_string_func(_args: hcl::eval::FuncArgs) -> Result<Value, String> {
    Ok(Value::String(String::new()))
}

fn get_env_func(args: hcl::eval::FuncArgs) -> Result<Value, String> {
    let mut iter = args.into_values().into_iter();
    let name = match iter.next() {
        Some(Value::String(s)) => s,
        _ => return Err("get_env: first argument must be a string".to_string()),
    };
    let default = iter.next().unwrap_or(Value::String(String::new()));
    match std::env::var(&name) {
        Ok(value) => Ok(Value::String(value)),
        Err(_) => Ok(default),
    }
}

fn value_to_display_string(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// Evaluate an HCL expression against the provided context.
///
/// Implements lazy `try()` semantics that `hcl-rs` does not provide on its
/// own: arguments to `try(...)` calls are evaluated one by one and the
/// first success wins. This matches terragrunt's behaviour where
/// `try(values.missing, "default")` returns `"default"` instead of erroring
/// because `values.missing` failed to evaluate.
pub fn evaluate_expression(expr: &hcl::Expression, ctx: &Context<'_>) -> Result<Value, String> {
    eval_lazy(expr, ctx, &[])
}

/// Iterator-variable bindings active during a single `for`-expression
/// iteration. Layered on top of `values`/`locals` when evaluating the
/// iteration's key/value/condition sub-expressions.
type IterBindings<'a> = [(&'a str, Value)];

/// Recursively evaluate an expression, intercepting every `try(...)` call so
/// its arguments are evaluated independently with errors caught. The
/// `iter_bindings` slice carries iterator variables introduced by enclosing
/// `for`-expressions so nested expressions evaluate against the correct
/// scope.
fn eval_lazy(expr: &hcl::Expression, ctx: &Context<'_>, iter_bindings: &IterBindings<'_>) -> Result<Value, String> {
    use hcl::Expression as E;

    // Fast path: no `try` calls and no iterator bindings to layer in -
    // delegate straight to hcl-rs.
    if iter_bindings.is_empty() && !contains_try(expr) {
        return expr.evaluate(ctx).map_err(|e| e.to_string());
    }

    match expr {
        E::FuncCall(fc) if is_bare_try(&fc.name) => {
            // Evaluate args one by one, swallowing errors and `null` so the
            // next candidate gets a chance. Mirrors the eager `try_func`
            // semantics callers already depend on, but with errors caught.
            let mut last_err: Option<String> = None;
            let mut last_value: Option<Value> = None;
            for arg in &fc.args {
                match eval_lazy(arg, ctx, iter_bindings) {
                    Ok(Value::Null) => last_value = Some(Value::Null),
                    Ok(value) => return Ok(value),
                    Err(e) => last_err = Some(e),
                }
            }
            if let Some(value) = last_value {
                return Ok(value);
            }
            Err(last_err.unwrap_or_else(|| "try: no arguments".to_string()))
        }
        // `for`-expressions need per-iteration bindings, which hcl-rs's
        // public API does not let us layer onto an existing context. We
        // therefore reproduce its iteration semantics and recurse with the
        // iterator variables added to `iter_bindings` so any nested `try`
        // resolves against them.
        E::ForExpr(fe) => eval_for_expr(fe, ctx, iter_bindings),
        // Composite expressions: rewrite child `try` calls in place, then
        // hand the rebuilt expression to hcl-rs for evaluation. When
        // iterator bindings are in scope we add them to a temporary child
        // context so traversals like `c.description` resolve.
        _ => {
            let rewritten = rewrite_try(expr, ctx, iter_bindings)?;
            if iter_bindings.is_empty() {
                rewritten.evaluate(ctx).map_err(|e| e.to_string())
            } else {
                let mut child = clone_context(ctx);
                for (name, value) in iter_bindings {
                    if let Ok(ident) = hcl::Identifier::new(*name) {
                        child.declare_var(ident, value.clone());
                    }
                }
                rewritten.evaluate(&child).map_err(|e| e.to_string())
            }
        }
    }
}

/// Rebuild a context that mirrors the one passed in. We have no public way
/// to clone or extend a `hcl::eval::Context`, so we re-derive a fresh
/// context with the same helpers registered. `values` and `local` are
/// recovered by evaluating bare identifiers against the source context.
fn clone_context(ctx: &Context<'_>) -> Context<'static> {
    let values = lookup_bare(ctx, "values").unwrap_or_else(|| Value::Object(hcl::Map::new()));
    let locals = lookup_bare(ctx, "local").unwrap_or_else(|| Value::Object(hcl::Map::new()));
    build_context(&values, &locals)
}

/// Resolve a bare variable in `ctx` by going through the expression
/// evaluator. Returns `None` if the variable is not declared.
fn lookup_bare(ctx: &Context<'_>, name: &str) -> Option<Value> {
    let expr: hcl::Expression = name.parse().ok()?;
    expr.evaluate(ctx).ok()
}

/// Evaluate a `for`-expression honouring lazy `try()` semantics inside its
/// key/value/condition sub-expressions.
fn eval_for_expr(
    fe: &hcl::expr::ForExpr,
    ctx: &Context<'_>,
    iter_bindings: &IterBindings<'_>,
) -> Result<Value, String> {
    let collection = eval_lazy(&fe.collection_expr, ctx, iter_bindings)?;
    let pairs: Vec<(Value, Value)> = match collection {
        Value::Array(items) => {
            items.into_iter().enumerate().map(|(idx, v)| (Value::Number(hcl::Number::from(idx as u64)), v)).collect()
        }
        Value::Object(map) => map.into_iter().map(|(k, v)| (Value::String(k), v)).collect(),
        other => return Err(format!("for: expected array or object, got {:?}", other)),
    };

    let value_var = fe.value_var.as_str();
    let key_var = fe.key_var.as_ref().map(|i| i.as_str());

    let produce_object = fe.key_expr.is_some();
    let mut out_array: Vec<Value> = Vec::new();
    let mut out_object: hcl::Map<String, Value> = hcl::Map::new();

    for (k, v) in pairs {
        // Build per-iteration bindings by layering the iterator vars over
        // whatever bindings the caller already supplied.
        let mut bindings: Vec<(&str, Value)> = iter_bindings.to_vec();
        if let Some(kv) = key_var {
            bindings.push((kv, k));
        }
        bindings.push((value_var, v));

        if let Some(cond) = &fe.cond_expr {
            match eval_lazy(cond, ctx, &bindings)? {
                Value::Bool(true) => {}
                Value::Bool(false) => continue,
                other => return Err(format!("for: condition must be boolean, got {:?}", other)),
            }
        }

        let value = eval_lazy(&fe.value_expr, ctx, &bindings)?;
        if produce_object {
            let key_expr = fe.key_expr.as_ref().expect("checked above");
            let key_val = eval_lazy(key_expr, ctx, &bindings)?;
            let key_str = match key_val {
                Value::String(s) => s,
                Value::Bool(b) => b.to_string(),
                Value::Number(n) => n.to_string(),
                other => return Err(format!("for: object key must be string-like, got {:?}", other)),
            };
            if fe.grouping {
                out_object
                    .entry(key_str)
                    .or_insert_with(|| Value::Array(Vec::new()))
                    .as_array_mut()
                    .expect("grouped values are arrays")
                    .push(value);
            } else {
                // Last-write-wins on duplicate keys. Terragrunt would error
                // here; we tolerate it to keep evaluation moving in the
                // best-effort warn-and-skip flow we already use elsewhere.
                out_object.insert(key_str, value);
            }
        } else {
            out_array.push(value);
        }
    }

    if produce_object {
        Ok(Value::Object(out_object))
    } else {
        Ok(Value::Array(out_array))
    }
}

fn is_bare_try(name: &hcl::expr::FuncName) -> bool {
    name.namespace.is_empty() && name.name.as_str() == "try"
}

/// Cheap structural check: does this expression (or any subexpression)
/// contain a `try(...)` call? Lets us skip rewriting when there is nothing
/// to do.
fn contains_try(expr: &hcl::Expression) -> bool {
    use hcl::Expression as E;
    match expr {
        E::FuncCall(fc) => is_bare_try(&fc.name) || fc.args.iter().any(contains_try),
        E::Array(items) => items.iter().any(contains_try),
        E::Object(map) => map.iter().any(|(k, v)| {
            let key_has = match k {
                hcl::expr::ObjectKey::Expression(e) => contains_try(e),
                _ => false,
            };
            key_has || contains_try(v)
        }),
        E::Traversal(t) => contains_try(&t.expr),
        E::Parenthesis(inner) => contains_try(inner),
        E::Conditional(c) => contains_try(&c.cond_expr) || contains_try(&c.true_expr) || contains_try(&c.false_expr),
        E::Operation(op) => match op.as_ref() {
            hcl::expr::Operation::Unary(u) => contains_try(&u.expr),
            hcl::expr::Operation::Binary(b) => contains_try(&b.lhs_expr) || contains_try(&b.rhs_expr),
        },
        E::ForExpr(f) => {
            contains_try(&f.collection_expr)
                || f.key_expr.as_ref().is_some_and(contains_try)
                || contains_try(&f.value_expr)
                || f.cond_expr.as_ref().is_some_and(contains_try)
        }
        _ => false,
    }
}

/// Walk `expr`, replacing every `try(...)` call with a literal expression of
/// its lazily evaluated result. The returned expression is safe to feed to
/// the standard evaluator.
///
/// `for`-expressions are pre-evaluated here (and inlined as literals) because
/// their sub-expressions reference iterator variables that only exist inside
/// `eval_for_expr`'s per-iteration scope.
fn rewrite_try(
    expr: &hcl::Expression,
    ctx: &Context<'_>,
    iter_bindings: &IterBindings<'_>,
) -> Result<hcl::Expression, String> {
    use hcl::Expression as E;

    if iter_bindings.is_empty() && !contains_try(expr) {
        return Ok(expr.clone());
    }

    match expr {
        E::FuncCall(fc) if is_bare_try(&fc.name) => {
            let value = eval_lazy(expr, ctx, iter_bindings)?;
            Ok(hcl::Expression::from(value))
        }
        E::FuncCall(fc) => {
            let mut rewritten = (**fc).clone();
            rewritten.args =
                fc.args.iter().map(|a| rewrite_try(a, ctx, iter_bindings)).collect::<Result<Vec<_>, _>>()?;
            Ok(E::FuncCall(Box::new(rewritten)))
        }
        E::Array(items) => {
            let new_items = items.iter().map(|i| rewrite_try(i, ctx, iter_bindings)).collect::<Result<Vec<_>, _>>()?;
            Ok(E::Array(new_items))
        }
        E::Object(map) => {
            let mut new_map = hcl::expr::Object::new();
            for (k, v) in map.iter() {
                let new_key = match k {
                    hcl::expr::ObjectKey::Expression(e) => {
                        hcl::expr::ObjectKey::Expression(rewrite_try(e, ctx, iter_bindings)?)
                    }
                    other => other.clone(),
                };
                new_map.insert(new_key, rewrite_try(v, ctx, iter_bindings)?);
            }
            Ok(E::Object(new_map))
        }
        E::Traversal(t) => {
            let mut rewritten = (**t).clone();
            rewritten.expr = rewrite_try(&t.expr, ctx, iter_bindings)?;
            Ok(E::Traversal(Box::new(rewritten)))
        }
        E::Parenthesis(inner) => Ok(E::Parenthesis(Box::new(rewrite_try(inner, ctx, iter_bindings)?))),
        E::Conditional(c) => {
            let mut rewritten = (**c).clone();
            rewritten.cond_expr = rewrite_try(&c.cond_expr, ctx, iter_bindings)?;
            rewritten.true_expr = rewrite_try(&c.true_expr, ctx, iter_bindings)?;
            rewritten.false_expr = rewrite_try(&c.false_expr, ctx, iter_bindings)?;
            Ok(E::Conditional(Box::new(rewritten)))
        }
        E::Operation(op) => {
            let new_op = match op.as_ref() {
                hcl::expr::Operation::Unary(u) => {
                    let mut nu = u.clone();
                    nu.expr = rewrite_try(&u.expr, ctx, iter_bindings)?;
                    hcl::expr::Operation::Unary(nu)
                }
                hcl::expr::Operation::Binary(b) => {
                    let mut nb = b.clone();
                    nb.lhs_expr = rewrite_try(&b.lhs_expr, ctx, iter_bindings)?;
                    nb.rhs_expr = rewrite_try(&b.rhs_expr, ctx, iter_bindings)?;
                    hcl::expr::Operation::Binary(nb)
                }
            };
            Ok(E::Operation(Box::new(new_op)))
        }
        E::ForExpr(_) => {
            // Resolve the entire for-expression now and splice in the result
            // as a literal so the surrounding evaluation sees a plain value.
            let value = eval_lazy(expr, ctx, iter_bindings)?;
            Ok(hcl::Expression::from(value))
        }
        _ => Ok(expr.clone()),
    }
}

/// Evaluate every attribute of every `locals { ... }` block.
///
/// Terragrunt resolves locals in topological order based on data
/// dependencies, not file order. We approximate this with a fixed-point
/// loop: each iteration evaluates the pending attributes; any whose
/// evaluation fails is retried in the next pass once more locals are
/// available. We stop when an iteration makes no progress.
///
/// Returns a `Value::Object` keyed by local name. Locals that never
/// resolve are silently dropped — callers depending on them surface
/// downstream errors.
pub fn evaluate_locals(body: &hcl::Body, values: &Value) -> Result<Value, String> {
    let mut pending: Vec<(String, &hcl::Expression)> = Vec::new();
    for block in body.blocks() {
        if block.identifier() != "locals" {
            continue;
        }
        for attr in block.body().attributes() {
            pending.push((attr.key().to_string(), attr.expr()));
        }
    }

    let mut locals_obj = hcl::Map::new();
    loop {
        let mut progressed = false;
        let mut remaining = Vec::with_capacity(pending.len());
        for (key, expr) in pending.drain(..) {
            let snapshot = Value::Object(locals_obj.clone());
            let ctx = build_context(values, &snapshot);
            match evaluate_expression(expr, &ctx) {
                Ok(value) => {
                    locals_obj.insert(key, value);
                    progressed = true;
                }
                Err(_) => remaining.push((key, expr)),
            }
        }
        pending = remaining;
        if pending.is_empty() || !progressed {
            break;
        }
    }

    Ok(Value::Object(locals_obj))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn obj(entries: &[(&str, Value)]) -> Value {
        let mut m = hcl::Map::new();
        for (k, v) in entries {
            m.insert(k.to_string(), v.clone());
        }
        Value::Object(m)
    }

    fn eval(src: &str, values: &Value, locals: &Value) -> Result<Value, String> {
        let expr: hcl::Expression = src.parse().map_err(|e: hcl::Error| e.to_string())?;
        let ctx = build_context(values, locals);
        evaluate_expression(&expr, &ctx)
    }

    #[test]
    fn bare_local_returns_full_locals_object() {
        let locals = obj(&[("x", Value::String("1".into())), ("y", Value::String("2".into()))]);
        let out = eval("local", &Value::Object(hcl::Map::new()), &locals).unwrap();
        assert_eq!(out, locals);
    }

    #[test]
    fn bare_values_returns_full_values_object() {
        let values = obj(&[("a", Value::String("x".into())), ("b", Value::String("y".into()))]);
        let out = eval("values", &values, &Value::Object(hcl::Map::new())).unwrap();
        assert_eq!(out, values);
    }

    #[test]
    fn evaluates_values_traversal() {
        let values = obj(&[("dep", Value::String("../sg".into()))]);
        let out = eval("values.dep", &values, &Value::Object(hcl::Map::new())).unwrap();
        assert_eq!(out, Value::String("../sg".into()));
    }

    #[test]
    fn merge_rhs_keys_win() {
        let values = Value::Object(hcl::Map::new());
        let locals = Value::Object(hcl::Map::new());
        let out = eval(r#"merge({ a = "1", b = "2" }, { b = "3", c = "4" })"#, &values, &locals).unwrap();
        let map = match out {
            Value::Object(m) => m,
            _ => panic!("expected object"),
        };
        assert_eq!(map.get("a"), Some(&Value::String("1".into())));
        assert_eq!(map.get("b"), Some(&Value::String("3".into())));
        assert_eq!(map.get("c"), Some(&Value::String("4".into())));
    }

    #[test]
    fn format_substitutes_string_verbs() {
        let values = obj(&[("region", Value::String("eu".into()))]);
        let out = eval(r#"format("svc-%s", values.region)"#, &values, &Value::Object(hcl::Map::new())).unwrap();
        assert_eq!(out, Value::String("svc-eu".into()));
    }

    #[test]
    fn coalesce_returns_first_non_null() {
        let out = eval("coalesce(null, null, \"x\")", &Value::Object(hcl::Map::new()), &Value::Object(hcl::Map::new()))
            .unwrap();
        assert_eq!(out, Value::String("x".into()));
    }

    #[test]
    fn try_returns_first_non_null() {
        let out =
            eval("try(null, \"fallback\")", &Value::Object(hcl::Map::new()), &Value::Object(hcl::Map::new())).unwrap();
        assert_eq!(out, Value::String("fallback".into()));
    }

    #[test]
    fn lazy_try_returns_fallback_when_first_arg_errors() {
        // `values.missing` raises an HCL error; lazy `try` should catch it
        // and fall through to the literal default.
        let out =
            eval(r#"try(values.missing, "default")"#, &Value::Object(hcl::Map::new()), &Value::Object(hcl::Map::new()))
                .unwrap();
        assert_eq!(out, Value::String("default".into()));
    }

    #[test]
    fn lazy_try_first_success_wins() {
        let values = obj(&[("present", Value::String("hit".into()))]);
        let out = eval(r#"try(values.present, "default")"#, &values, &Value::Object(hcl::Map::new())).unwrap();
        assert_eq!(out, Value::String("hit".into()));
    }

    #[test]
    fn lazy_try_nested_in_merge() {
        let out = eval(
            r#"merge({ a = "1" }, try(values.missing, { b = "2" }))"#,
            &Value::Object(hcl::Map::new()),
            &Value::Object(hcl::Map::new()),
        )
        .unwrap();
        let map = match out {
            Value::Object(m) => m,
            _ => panic!("expected object"),
        };
        assert_eq!(map.get("a"), Some(&Value::String("1".into())));
        assert_eq!(map.get("b"), Some(&Value::String("2".into())));
    }

    #[test]
    fn lookup_returns_value_when_present() {
        let out = eval(
            r#"lookup({ a = "1" }, "a", "default")"#,
            &Value::Object(hcl::Map::new()),
            &Value::Object(hcl::Map::new()),
        )
        .unwrap();
        assert_eq!(out, Value::String("1".into()));
    }

    #[test]
    fn lookup_returns_default_when_missing() {
        let out = eval(
            r#"lookup({ a = "1" }, "missing", "default")"#,
            &Value::Object(hcl::Map::new()),
            &Value::Object(hcl::Map::new()),
        )
        .unwrap();
        assert_eq!(out, Value::String("default".into()));
    }

    #[test]
    fn tostring_converts_number() {
        let values = obj(&[("n", Value::Number(hcl::Number::from(7)))]);
        let out = eval("tostring(values.n)", &values, &Value::Object(hcl::Map::new())).unwrap();
        assert_eq!(out, Value::String("7".into()));
    }

    #[test]
    fn missing_traversal_errors() {
        let err = eval("values.missing", &Value::Object(hcl::Map::new()), &Value::Object(hcl::Map::new())).unwrap_err();
        assert!(!err.is_empty(), "expected an error message");
    }

    #[test]
    fn get_env_returns_default_when_unset() {
        let out = eval(
            r#"get_env("UNLIKELY_VAR_NAME_TG_DAG", "fallback")"#,
            &Value::Object(hcl::Map::new()),
            &Value::Object(hcl::Map::new()),
        )
        .unwrap();
        assert_eq!(out, Value::String("fallback".into()));
    }

    #[test]
    fn evaluates_locals_in_order() {
        let body: hcl::Body = hcl::from_str(
            r#"
            locals {
              region = "eu"
              name   = "svc-${local.region}"
            }
        "#,
        )
        .unwrap();
        let locals = evaluate_locals(&body, &Value::Object(hcl::Map::new())).unwrap();
        let map = match locals {
            Value::Object(m) => m,
            _ => panic!("expected object"),
        };
        assert_eq!(map.get("region"), Some(&Value::String("eu".into())));
        assert_eq!(map.get("name"), Some(&Value::String("svc-eu".into())));
    }

    #[test]
    fn for_tuple_expression_doubles_each_element() {
        let out =
            eval("[for x in [1, 2, 3] : x * 2]", &Value::Object(hcl::Map::new()), &Value::Object(hcl::Map::new()))
                .unwrap();
        assert_eq!(
            out,
            Value::Array(vec![
                Value::Number(hcl::Number::from(2)),
                Value::Number(hcl::Number::from(4)),
                Value::Number(hcl::Number::from(6)),
            ])
        );
    }

    #[test]
    fn for_tuple_with_filter() {
        let out = eval(
            "[for x in [1, 2, 3, 4] : x if x > 2]",
            &Value::Object(hcl::Map::new()),
            &Value::Object(hcl::Map::new()),
        )
        .unwrap();
        assert_eq!(out, Value::Array(vec![Value::Number(hcl::Number::from(3)), Value::Number(hcl::Number::from(4))]));
    }

    #[test]
    fn for_object_over_map_yields_renamed_keys() {
        let customers = obj(&[
            ("alice", obj(&[("age", Value::Number(hcl::Number::from(30)))])),
            ("bob", obj(&[("age", Value::Number(hcl::Number::from(25)))])),
        ]);
        let locals = obj(&[("customers", customers)]);
        let out =
            eval(r#"{ for name, c in local.customers : name => c.age }"#, &Value::Object(hcl::Map::new()), &locals)
                .unwrap();
        let map = match out {
            Value::Object(m) => m,
            _ => panic!("expected object"),
        };
        assert_eq!(map.get("alice"), Some(&Value::Number(hcl::Number::from(30))));
        assert_eq!(map.get("bob"), Some(&Value::Number(hcl::Number::from(25))));
    }

    #[test]
    fn for_object_with_interpolated_key() {
        let customers = obj(&[("alice", Value::Object(hcl::Map::new())), ("bob", Value::Object(hcl::Map::new()))]);
        let locals = obj(&[("customers", customers)]);
        let out = eval(
            r#"{ for name, _ in local.customers : "user-${name}" => name }"#,
            &Value::Object(hcl::Map::new()),
            &locals,
        )
        .unwrap();
        let map = match out {
            Value::Object(m) => m,
            _ => panic!("expected object"),
        };
        assert_eq!(map.get("user-alice"), Some(&Value::String("alice".into())));
        assert_eq!(map.get("user-bob"), Some(&Value::String("bob".into())));
    }

    #[test]
    fn for_tuple_keys_of_map() {
        let out = eval(
            r#"[for k, _ in { a = 1, b = 2 } : k]"#,
            &Value::Object(hcl::Map::new()),
            &Value::Object(hcl::Map::new()),
        )
        .unwrap();
        let array = match out {
            Value::Array(a) => a,
            _ => panic!("expected array"),
        };
        // Order may not be guaranteed: assert both keys are present.
        assert_eq!(array.len(), 2);
        assert!(array.contains(&Value::String("a".into())));
        assert!(array.contains(&Value::String("b".into())));
    }

    #[test]
    fn for_body_calls_try_with_missing_values_field() {
        // Iterator var captures a value; nested `try(values.missing, x)`
        // must fall back to `x` instead of erroring on the missing field.
        let out = eval(
            r#"[for x in [1, 2, 3] : try(values.missing, x)]"#,
            &Value::Object(hcl::Map::new()),
            &Value::Object(hcl::Map::new()),
        )
        .unwrap();
        assert_eq!(
            out,
            Value::Array(vec![
                Value::Number(hcl::Number::from(1)),
                Value::Number(hcl::Number::from(2)),
                Value::Number(hcl::Number::from(3)),
            ])
        );
    }

    #[test]
    fn for_body_with_try_falls_back_per_iteration() {
        // Mirrors the example `keycloak.organizations` pattern: each iteration
        // calls try() on a possibly-missing field of the iterator variable.
        let customers = obj(&[
            ("alice", obj(&[("description", Value::String("alice desc".into()))])),
            ("bob", Value::Object(hcl::Map::new())),
        ]);
        let locals = obj(&[("customers", customers)]);
        let out = eval(
            r#"{ for name, c in local.customers : name => try(c.description, "ACME customer: ${name}") }"#,
            &Value::Object(hcl::Map::new()),
            &locals,
        )
        .unwrap();
        let map = match out {
            Value::Object(m) => m,
            _ => panic!("expected object"),
        };
        assert_eq!(map.get("alice"), Some(&Value::String("alice desc".into())));
        assert_eq!(map.get("bob"), Some(&Value::String("ACME customer: bob".into())));
    }

    #[test]
    fn for_expression_in_merge_via_local() {
        // Mirrors the example `keycloak.groups` pattern: a for-object is the
        // second argument to merge(), and references local.customers.
        let body: hcl::Body = hcl::from_str(
            r#"
            locals {
              customers = { alice = {}, bob = {} }
              groups    = merge({ static = "x" }, { for name, _ in local.customers : "p/${name}" => name })
            }
        "#,
        )
        .unwrap();
        let locals = evaluate_locals(&body, &Value::Object(hcl::Map::new())).unwrap();
        let map = match locals {
            Value::Object(m) => m,
            _ => panic!("expected object"),
        };
        let groups = match map.get("groups").expect("groups present") {
            Value::Object(m) => m.clone(),
            _ => panic!("expected object"),
        };
        assert_eq!(groups.get("static"), Some(&Value::String("x".into())));
        assert_eq!(groups.get("p/alice"), Some(&Value::String("alice".into())));
        assert_eq!(groups.get("p/bob"), Some(&Value::String("bob".into())));
    }

    #[test]
    fn evaluates_merge_with_locals_and_values() {
        let body: hcl::Body = hcl::from_str(
            r#"
            locals {
              common = { env = "prod" }
            }
        "#,
        )
        .unwrap();
        let values = obj(&[("region", Value::String("eu".into()))]);
        let locals = evaluate_locals(&body, &values).unwrap();
        let out = eval(
            r#"merge({ a = "../base" }, local.common, { b = format("svc-%s", values.region) })"#,
            &values,
            &locals,
        )
        .unwrap();
        let map = match out {
            Value::Object(m) => m,
            _ => panic!("expected object"),
        };
        assert_eq!(map.get("a"), Some(&Value::String("../base".into())));
        assert_eq!(map.get("env"), Some(&Value::String("prod".into())));
        assert_eq!(map.get("b"), Some(&Value::String("svc-eu".into())));
    }
}
