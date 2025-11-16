use std::fmt::Debug;

use digest::Digest;
use proc_macro2::TokenStream;
use quote::{format_ident, quote};
use syn::{
    parenthesized,
    parse::{Parse, ParseStream},
    parse_macro_input, parse_quote,
    spanned::Spanned,
    token, Ident, ItemFn, Path, ReturnType, Signature,
};

enum SplitArgPart {
    Ident(Ident),
    Argument {
        arg_name: Ident,
        _paren_token: token::Paren,
        tts: TokenStream,
    },
}
impl Debug for SplitArgPart {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SplitArgPart::Ident(ident) => write!(f, "Ident({})", ident),
            SplitArgPart::Argument { arg_name, tts, .. } => {
                write!(f, "Argument({} ({}))", arg_name, tts)
            }
        }
    }
}
impl Parse for SplitArgPart {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        if input.peek2(token::Paren) {
            let arg_name: Ident = input.parse()?;
            let content;
            let _paren_token = parenthesized!(content in input);
            let tts: TokenStream = content.parse()?;
            Ok(SplitArgPart::Argument {
                arg_name,
                _paren_token,
                tts,
            })
        } else {
            let ident: Ident = input.parse()?;
            Ok(SplitArgPart::Ident(ident))
        }
    }
}

/// Arguments for `#[split(...)]` macro.
/// All argument shouuld be in format of `arg_name(...)`,
/// `module_name` is allowed to be the first positional argument.
/// No duplicate arguments are allowed.
///
/// Example:
/// #[split(my_module, wrap_return_with(SomeWrapper), seed(1234))]
struct SplitArgs {
    /// The name
    module_name: Ident,
    /// Wrap exported fn return expression with constructor from this path
    wrap_return_with: Option<Path>,
    /// As wrap_return_with but only wraps `async block` inside async fns.
    /// If used with `wrap_return_with` - inner body is wrapped with `wrap_return_with` and then
    /// the resulting future is wrapped with `async_wrap_return_with`.
    async_wrap_return_with: Option<Path>,
    /// Add tokens as a prefix for unique id.
    /// Usefull to differentiate multiple split functions with conflict module_name/fn_name.
    uniq_id_prefix: Option<Ident>,

    /// The path to the custom loader.
    /// In format of `custom_loader(some::path::to::loader::function)`, this function should be async and have signature:
    /// `async fn loader(module_id: ModuleId) -> bool`
    custom_loader: Option<Path>,

    /// The url to load the module from.
    /// Can be relative or absolute.
    with_url: Option<syn::LitStr>,
}

impl Parse for SplitArgs {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        macro_rules! ensure_is_empty {
            ($arg_name:expr) => {
                if $arg_name.is_some() {
                    return Err(syn::Error::new(
                        $arg_name.unwrap().span(),
                        "Argument specified multiple times, first specified at",
                    ));
                }
            };
        }
        let mut module_name = None;
        let mut wrap_return_with: Option<Path> = None;
        let mut async_wrap_return_with: Option<Path> = None;
        let mut uniq_id_prefix: Option<Ident> = None;
        let mut custom_loader: Option<Path> = None;
        let mut module_url: Option<syn::LitStr> = None;

        while !input.is_empty() {
            let part: SplitArgPart = input.parse()?;
            match part {
                SplitArgPart::Ident(ident) => {
                    module_name = Some(ident);
                }
                SplitArgPart::Argument { arg_name, tts, .. } => {
                    match arg_name.to_string().as_str() {
                        "module_name" => {
                            ensure_is_empty!(module_name);
                            module_name = Some(syn::parse2(tts)?);
                        }
                        "wrap_return_with" => {
                            ensure_is_empty!(wrap_return_with);
                            let path: Path = syn::parse2(tts)?;
                            wrap_return_with = Some(path);
                        }
                        "async_wrap_return_with" => {
                            ensure_is_empty!(async_wrap_return_with);
                            async_wrap_return_with = Some(syn::parse2(tts)?);
                        }
                        "uniq_id_prefix" => {
                            ensure_is_empty!(uniq_id_prefix);
                            uniq_id_prefix = Some(syn::parse2(tts)?);
                        }
                        "custom_loader" => {
                            ensure_is_empty!(custom_loader);
                            let path: Path = syn::parse2(tts)?;
                            custom_loader = Some(path);
                        }
                        "module_url" => {
                            ensure_is_empty!(module_url);
                            module_url = Some(syn::parse2(tts)?);
                        }
                        _ => {
                            return Err(syn::Error::new(
                                arg_name.span(),
                                format!("Unknown argument name: {}", arg_name),
                            ));
                        }
                    }
                }
            }
            if !input.peek(token::Comma) {
                break;
            }
            let _comma: token::Comma = input.parse()?;
        }

        Ok(Self {
            // module name is required
            module_name: module_name
                .ok_or_else(|| syn::Error::new(input.span(), "Expected module name"))?,
            wrap_return_with,
            async_wrap_return_with,
            uniq_id_prefix,
            custom_loader,
            with_url: module_url,
        })
    }
}

#[proc_macro_attribute]
pub fn split(
    args: proc_macro::TokenStream,
    input: proc_macro::TokenStream,
) -> proc_macro::TokenStream {
    let file = file_name(input.clone());
    let args = parse_macro_input!(args as SplitArgs);
    let item_fn = parse_macro_input!(input as ItemFn);
    split_inner(args, item_fn, &file).into()
}

fn file_name(input: proc_macro::TokenStream) -> String {
    input
        .into_iter()
        .next()
        .map(|tt| tt.span().file())
        .unwrap_or_default()
}

fn split_inner(args: SplitArgs, item_fn: ItemFn, file_name: &str) -> TokenStream {
    let SplitArgs {
        module_name,
        wrap_return_with,
        uniq_id_prefix,
        custom_loader,
        async_wrap_return_with,
        with_url,
    } = args;

    let vis = item_fn.vis;
    let name = &item_fn.sig.ident;

    let uniq_id_prefix = if let Some(uniq_id_prefix) = uniq_id_prefix {
        uniq_id_prefix
    } else {
        Ident::new("u", proc_macro2::Span::call_site())
    };
    // Unique identifier can help avoid name clashes when the same function is defined in multiple modules.
    // But using span for this make incremental extraction impossible - since a lot of symbols changes each time.
    // Instead of span we use file path. But currently this not fix a case where multiple functions have the same name in the same file.
    let unique_identifier =
        base16::encode_lower(&sha2::Sha256::digest(format!("{name} {file_name}",))[..16]);

    let impl_import_ident = format_ident!(
        "__wamex_00{module_name}00_import_{name}_{uniq_id_prefix}{unique_identifier}"
    );
    let impl_export_ident = format_ident!(
        "__wamex_00{module_name}00_export_{name}_{uniq_id_prefix}{unique_identifier}"
    );

    let mut import_sig = Signature {
        ident: impl_import_ident.clone(),
        asyncness: None,
        ..item_fn.sig.clone()
    };
    let mut export_sig = Signature {
        ident: impl_export_ident.clone(),
        asyncness: None,
        ..item_fn.sig.clone()
    };

    let original_body_stmts = &item_fn.block.stmts;
    let mut body: TokenStream = if let Some(wrapper) = wrap_return_with {
        quote! {
            {
                let __wamex_result = (|| async move {
                    #(#original_body_stmts)*
                })().await;
                #wrapper ( __wamex_result )
            }
        }
    } else {
        quote! { #(#original_body_stmts)* }
    };

    let is_async = item_fn.sig.asyncness.is_some();
    // Convert async fn to fn returning Pin<Box<dyn Future>>
    if is_async {
        let ty = match &item_fn.sig.output {
            ReturnType::Default => quote! { () },
            ReturnType::Type(_, ty) => quote! { #ty },
        };
        let async_output: ReturnType = parse_quote! {
            -> ::core::pin::Pin<Box<dyn ::core::future::Future<Output = #ty>>>
        };
        export_sig.output = async_output.clone();
        import_sig.output = async_output;

        body = if let Some(async_wrapper) = async_wrap_return_with {
            quote! {
                Box::pin(#async_wrapper ( async move {
                    #body
                }))
            }
        } else {
            quote! {
                Box::pin(async move {
                    #body
                })
            }
        }
    }

    let mut wrapper_sig = item_fn.sig;
    wrapper_sig.asyncness = Some(Default::default());
    let mut args = Vec::new();
    for (i, param) in wrapper_sig.inputs.iter_mut().enumerate() {
        match param {
            syn::FnArg::Typed(pat_type) => {
                let param_ident = format_ident!("__wamex_arg_{i}");
                args.push(param_ident.clone());
                pat_type.pat = Box::new(syn::Pat::Ident(syn::PatIdent {
                    attrs: vec![],
                    by_ref: None,
                    mutability: None,
                    ident: param_ident,
                    subpat: None,
                }));
            }
            syn::FnArg::Receiver(_) => {
                args.push(format_ident!("self"));
            }
        }
    }

    let attrs = item_fn.attrs;
    let import_call_expr = if is_async {
        quote! {
            #impl_import_ident( #(#args),* ).await
        }
    } else {
        quote! {
            #impl_import_ident( #(#args),* )
        }
    };

    let loader = if let Some(custom_loader) = custom_loader {
        quote! { #custom_loader }
    } else {
        quote! { ::wamex::load }
    };

    let module_name_str = module_name.to_string();

    let module_id = if let Some(url) = with_url {
        quote! {
            ::wamex::ModuleId::with_url(#module_name_str, #url)
        }
    } else {
        quote! {
            ::wamex::ModuleId::new(#module_name_str)
        }
    };

    quote! {
        #(#attrs)*
        #vis #wrapper_sig {

            // This import will be replaced so we can place any module name here
            #[link(wasm_import_module = "./__wamex_link.rs")]
            extern "C" {

                #[allow(improper_ctypes)]
                #[no_mangle]
                #import_sig;
            }
            #loader(#module_id).await;

            #[allow(improper_ctypes_definitions)]
            #[no_mangle]
            pub extern "C" #export_sig {
                #body
            }

            unsafe { #import_call_expr }
        }
    }
    .into()
}
