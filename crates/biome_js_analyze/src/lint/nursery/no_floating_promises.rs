use biome_analyze::{
    context::RuleContext, declare_lint_rule, FixKind, Rule, RuleDiagnostic, RuleSource,
};
use biome_console::markup;
use biome_js_factory::make;
use biome_js_semantic::SemanticModel;
use biome_js_syntax::{
    binding_ext::AnyJsBindingDeclaration, AnyJsExpression, AnyJsName, AnyTsName, AnyTsReturnType,
    AnyTsType, AnyTsVariableAnnotation, JsArrowFunctionExpression, JsCallExpression,
    JsExpressionStatement, JsFunctionDeclaration, JsMethodClassMember, JsMethodObjectMember,
    JsStaticMemberExpression, JsSyntaxKind, JsVariableDeclarator, TsReturnTypeAnnotation,
};
use biome_rowan::{AstNode, AstSeparatedList, BatchMutationExt, SyntaxNodeCast, TriviaPieceKind};

use crate::{services::semantic::Semantic, JsRuleAction};

declare_lint_rule! {
    /// Require Promise-like statements to be handled appropriately.
    ///
    /// A "floating" `Promise` is one that is created without any code set up to handle any errors it might throw.
    /// Floating Promises can lead to several issues, including improperly sequenced operations, unhandled Promise rejections, and other unintended consequences.
    ///
    /// This rule will report Promise-valued statements that are not treated in one of the following ways:
    /// - Calling its `.then()` method with two arguments
    /// - Calling its `.catch()` method with one argument
    /// - `await`ing it
    /// - `return`ing it
    /// - `void`ing it
    ///
    /// :::caution
    /// ## Important notes
    ///
    /// This rule is a work in progress, and is only partially implemented.
    /// Progress is being tracked in the following GitHub issue: https://github.com/biomejs/biome/issues/3187
    /// :::
    ///
    /// ## Examples
    ///
    /// ### Invalid
    ///
    /// ```ts,expect_diagnostic
    /// async function returnsPromise(): Promise<string> {
    ///   return 'value';
    /// }
    /// returnsPromise().then(() => {});
    /// ```
    ///
    /// ```ts,expect_diagnostic
    /// const returnsPromise = async (): Promise<string> => {
    ///   return 'value';
    /// }
    /// async function returnsPromiseInAsyncFunction() {
    ///   returnsPromise().then(() => {});
    /// }
    /// ```
    ///
    /// ### Valid
    ///
    /// ```ts
    /// async function returnsPromise(): Promise<string> {
    ///   return 'value';
    /// }
    ///
    /// await returnsPromise();
    ///
    /// void returnsPromise();
    ///
    /// // Calling .then() with two arguments
    /// returnsPromise().then(
    ///   () => {},
    ///   () => {},
    /// );
    ///
    /// // Calling .catch() with one argument
    /// returnsPromise().catch(() => {});
    /// ```
    ///
    pub NoFloatingPromises {
        version: "next",
        name: "noFloatingPromises",
        language: "ts",
        recommended: false,
        sources: &[RuleSource::EslintTypeScript("no-floating-promises")],
        fix_kind: FixKind::Unsafe,
    }
}

impl Rule for NoFloatingPromises {
    type Query = Semantic<JsExpressionStatement>;
    type State = ();
    type Signals = Option<Self::State>;
    type Options = ();

    fn run(ctx: &RuleContext<Self>) -> Self::Signals {
        let node = ctx.query();
        let model = ctx.model();
        let expression = node.expression().ok()?;
        if let AnyJsExpression::JsCallExpression(js_call_expression) = expression {
            let Ok(any_js_expression) = js_call_expression.callee() else {
                return None;
            };

            if !is_callee_a_promise(&any_js_expression, model) {
                return None;
            }

            if is_handled_promise(&js_call_expression) {
                return None;
            }

            return Some(());
        }
        None
    }

    fn diagnostic(ctx: &RuleContext<Self>, _state: &Self::State) -> Option<RuleDiagnostic> {
        let node = ctx.query();
        Some(
            RuleDiagnostic::new(
                rule_category!(),
                node.range(),
                markup! {
                    "A \"floating\" Promise was found, meaning it is not properly handled and could lead to ignored errors or unexpected behavior."
                },
            )
            .note(markup! {
                "This happens when a Promise is not awaited, lacks a `.catch` or `.then` rejection handler, or is not explicitly ignored using the `void` operator."
            })
        )
    }

    fn action(ctx: &RuleContext<Self>, _: &Self::State) -> Option<JsRuleAction> {
        let node = ctx.query();

        if !is_in_async_function(node) {
            return None;
        }

        let expression = node.expression().ok()?;
        let mut mutation = ctx.root().begin();
        let await_expression = AnyJsExpression::JsAwaitExpression(make::js_await_expression(
            make::token(JsSyntaxKind::AWAIT_KW)
                .with_trailing_trivia([(TriviaPieceKind::Whitespace, " ")]),
            expression.clone().trim_leading_trivia()?,
        ));

        mutation.replace_node(expression, await_expression);
        Some(JsRuleAction::new(
            ctx.metadata().action_category(ctx.category(), ctx.group()),
            ctx.metadata().applicability(),
            markup! { "Add await operator." }.to_owned(),
            mutation,
        ))
    }
}

/// Checks if the callee of a JavaScript expression is a promise.
///
/// This function inspects the callee of a given JavaScript expression to determine
/// if it is a promise. It returns `true` if the callee is a promise, otherwise `false`.
///
/// The function works by finding the binding of the callee and checking if it is a promise.
///
/// # Arguments
///
/// * `callee` - A reference to an `AnyJsExpression` representing the callee to check.
/// * `model` - A reference to the `SemanticModel` used for resolving bindings.
///
/// # Returns
///
/// * `true` if the callee is a promise.
/// * `false` otherwise.
///
/// # Examples
///
/// Example JavaScript code that would return `true`:
/// ```typescript
/// async function returnsPromise(): Promise<string> {
///     return "value";
/// }
///
/// returnsPromise().then(() => {});
/// ```
///
/// Example JavaScript code that would return `false`:
/// ```typescript
/// function doesNotReturnPromise() {
///     return 42;
/// }
///
/// doesNotReturnPromise().then(() => {});
/// ```
fn is_callee_a_promise(callee: &AnyJsExpression, model: &SemanticModel) -> bool {
    match callee {
        AnyJsExpression::JsIdentifierExpression(ident_expr) => {
            let Some(reference) = ident_expr.name().ok() else {
                return false;
            };

            let Some(binding) = model.binding(&reference) else {
                return false;
            };

            let Some(any_js_binding_decl) = binding.tree().declaration() else {
                return false;
            };
            match any_js_binding_decl {
                AnyJsBindingDeclaration::JsFunctionDeclaration(func_decl) => {
                    is_function_a_promise(&func_decl)
                }
                AnyJsBindingDeclaration::JsVariableDeclarator(js_var_decl) => {
                    is_variable_initializer_a_promise(&js_var_decl)
                        || is_variable_annotation_a_promise(&js_var_decl)
                }
                _ => false,
            }
        }
        AnyJsExpression::JsStaticMemberExpression(static_member_expr) => {
            is_member_expression_callee_a_promise(static_member_expr, model)
        }
        _ => false,
    }
}

fn is_function_a_promise(func_decl: &JsFunctionDeclaration) -> bool {
    func_decl.async_token().is_some()
        || is_return_type_a_promise(func_decl.return_type_annotation())
}

/// Checks if a TypeScript return type annotation is a `Promise`.
///
/// This function inspects the return type annotation of a TypeScript function to determine
/// if it is a `Promise`. It returns `true` if the return type annotation is `Promise`, otherwise `false`.
///
/// # Arguments
///
/// * `return_type` - An optional `TsReturnTypeAnnotation` to check.
///
/// # Returns
///
/// * `true` if the return type annotation is `Promise`.
/// * `false` otherwise.
///
/// # Examples
///
/// Example TypeScript code that would return `true`:
/// ```typescript
/// async function returnsPromise(): Promise<void> {}
/// ```
///
/// Example TypeScript code that would return `false`:
/// ```typescript
/// function doesNotReturnPromise(): void {}
/// ```
fn is_return_type_a_promise(return_type: Option<TsReturnTypeAnnotation>) -> bool {
    return_type
        .and_then(|ts_return_type_anno| ts_return_type_anno.ty().ok())
        .and_then(|any_ts_return_type| match any_ts_return_type {
            AnyTsReturnType::AnyTsType(any_ts_type) => Some(any_ts_type),
            _ => None,
        })
        .and_then(|any_ts_type| match any_ts_type {
            AnyTsType::TsReferenceType(reference_type) => Some(reference_type),
            _ => None,
        })
        .and_then(|reference_type| reference_type.name().ok())
        .and_then(|name| match name {
            AnyTsName::JsReferenceIdentifier(identifier) => Some(identifier),
            _ => None,
        })
        .map_or(false, |reference| reference.has_name("Promise"))
}

/// Checks if a `JsCallExpression` is a handled Promise-like expression.
/// - Calling its .then() with two arguments
/// - Calling its .catch() with one argument
///
/// Example TypeScript code that would return `true`:
///
/// ```typescript
/// const promise: Promise<unknown> = new Promise((resolve, reject) => resolve('value'));
/// promise.then(() => "aaa", () => null).finally(() => null)
///
/// const promise: Promise<unknown> = new Promise((resolve, reject) => resolve('value'));
/// promise.then(() => "aaa").catch(() => null).finally(() => null)
/// ```
fn is_handled_promise(js_call_expression: &JsCallExpression) -> bool {
    let Ok(expr) = js_call_expression.callee() else {
        return false;
    };

    let AnyJsExpression::JsStaticMemberExpression(static_member_expr) = expr else {
        return false;
    };

    let Ok(AnyJsName::JsName(name)) = static_member_expr.member() else {
        return false;
    };

    let name = name.to_string();

    if name == "finally" {
        if let Ok(expr) = static_member_expr.object() {
            if let Some(callee) = expr.as_js_call_expression() {
                return is_handled_promise(callee);
            }
        }
    }
    if name == "catch" {
        if let Ok(call_args) = js_call_expression.arguments() {
            // just checking if there are any arguments, not if it's a function for simplicity
            if call_args.args().len() > 0 {
                return true;
            }
        }
    }
    if name == "then" {
        if let Ok(call_args) = js_call_expression.arguments() {
            // just checking arguments have a reject function from length
            if call_args.args().len() >= 2 {
                return true;
            }
        }
    }
    false
}

/// Checks if the callee of a `JsStaticMemberExpression` is a promise expression.
///
/// This function inspects the callee of a `JsStaticMemberExpression` to determine
/// if it is a promise expression. It returns `true` if the callee is a promise expression,
/// otherwise `false`.
///
/// # Arguments
///
/// * `static_member_expr` - A reference to a `JsStaticMemberExpression` to check.
/// * `model` - A reference to the `SemanticModel` used for resolving bindings.
///
/// # Returns
///
/// * `true` if the callee is a promise expression.
/// * `false` otherwise.
///
/// # Examples
///
/// Example TypeScript code that would return `true`:
/// ```typescript
/// async function returnsPromise(): Promise<void> {}
///
/// returnsPromise().then(() => null).catch(() => {});
/// ```
///
/// Example TypeScript code that would return `false`:
/// ```typescript
/// function doesNotReturnPromise(): void {}
///
/// doesNotReturnPromise().then(() => null).catch(() => {});
/// ```
fn is_member_expression_callee_a_promise(
    static_member_expr: &JsStaticMemberExpression,
    model: &SemanticModel,
) -> bool {
    let Ok(expr) = static_member_expr.object() else {
        return false;
    };

    let AnyJsExpression::JsCallExpression(js_call_expr) = expr else {
        return false;
    };

    let Ok(callee) = js_call_expr.callee() else {
        return false;
    };

    is_callee_a_promise(&callee, model)
}

/// Checks if the given `JsExpressionStatement` is within an async function.
///
/// This function traverses up the syntax tree from the given expression node
/// to find the nearest function and checks if it is an async function. It
/// supports arrow functions, function declarations, class methods, and object
/// methods.
///
/// # Arguments
///
/// * `node` - A reference to a `JsExpressionStatement` to check.
///
/// # Returns
///
/// * `true` if the expression is within an async function.
/// * `false` otherwise.
fn is_in_async_function(node: &JsExpressionStatement) -> bool {
    node.syntax()
        .ancestors()
        .find_map(|ancestor| match ancestor.kind() {
            JsSyntaxKind::JS_ARROW_FUNCTION_EXPRESSION => ancestor
                .cast::<JsArrowFunctionExpression>()
                .and_then(|func| func.async_token()),
            JsSyntaxKind::JS_FUNCTION_DECLARATION => ancestor
                .cast::<JsFunctionDeclaration>()
                .and_then(|func| func.async_token()),
            JsSyntaxKind::JS_METHOD_CLASS_MEMBER => ancestor
                .cast::<JsMethodClassMember>()
                .and_then(|method| method.async_token()),
            JsSyntaxKind::JS_METHOD_OBJECT_MEMBER => ancestor
                .cast::<JsMethodObjectMember>()
                .and_then(|method| method.async_token()),
            _ => None,
        })
        .is_some()
}

/// Checks if the initializer of a `JsVariableDeclarator` is an async function.
///
/// Example TypeScript code that would return `true`:
///
/// ```typescript
/// const returnsPromise = async (): Promise<string> => {
///   return 'value';
/// }
///
/// const returnsPromise = async function (): Promise<string> {
///   return 'value'
/// }
/// ```
fn is_variable_initializer_a_promise(js_variable_declarator: &JsVariableDeclarator) -> bool {
    let Some(initializer_clause) = &js_variable_declarator.initializer() else {
        return false;
    };
    let Ok(expr) = initializer_clause.expression() else {
        return false;
    };
    match expr {
        AnyJsExpression::JsArrowFunctionExpression(arrow_func) => {
            arrow_func.async_token().is_some()
                || is_return_type_a_promise(arrow_func.return_type_annotation())
        }
        AnyJsExpression::JsFunctionExpression(func_expr) => {
            func_expr.async_token().is_some()
                || is_return_type_a_promise(func_expr.return_type_annotation())
        }
        _ => false,
    }
}

/// Checks if a `JsVariableDeclarator` has a TypeScript type annotation of `Promise`.
///
///
/// Example TypeScript code that would return `true`:
/// ```typescript
/// const returnsPromise: () => Promise<string> = () => {
///   return Promise.resolve("value")
/// }
/// ```
fn is_variable_annotation_a_promise(js_variable_declarator: &JsVariableDeclarator) -> bool {
    js_variable_declarator
        .variable_annotation()
        .and_then(|anno| match anno {
            AnyTsVariableAnnotation::TsTypeAnnotation(type_anno) => Some(type_anno),
            _ => None,
        })
        .and_then(|ts_type_anno| ts_type_anno.ty().ok())
        .and_then(|any_ts_type| match any_ts_type {
            AnyTsType::TsFunctionType(func_type) => {
                func_type
                    .return_type()
                    .ok()
                    .and_then(|return_type| match return_type {
                        AnyTsReturnType::AnyTsType(AnyTsType::TsReferenceType(ref_type)) => {
                            ref_type.name().ok().map(|name| match name {
                                AnyTsName::JsReferenceIdentifier(identifier) => {
                                    identifier.has_name("Promise")
                                }
                                _ => false,
                            })
                        }
                        _ => None,
                    })
            }
            _ => None,
        })
        .unwrap_or(false)
}
