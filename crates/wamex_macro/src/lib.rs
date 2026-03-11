use std::fmt::Debug;

use proc_macro2::TokenStream;
use quote::{format_ident, quote};
use sha2::{Digest, Sha256};
use syn::{
    parenthesized,
    parse::{Parse, ParseStream, Parser},
    parse_macro_input, parse_quote,
    punctuated::Punctuated,
    spanned::Spanned,
    token, FnArg, Ident, ItemFn, Path, ReturnType, Signature, Token,
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
    /// The name of the module to split.
    pub module_name: Ident,
    /// Wrap exported fn return expression with constructor from this path.
    pub wrap_return_with: Option<Path>,
    /// As wrap_return_with but only wraps `async block` inside async fns.
    /// If used with `wrap_return_with` - inner body is wrapped with `wrap_return_with` and then
    /// the resulting future is wrapped with `async_wrap_return_with`.
    pub async_wrap_return_with: Option<Path>,
    ///
    /// `async fn` is converted to regular with `Box<Pin<dyn Future<Output=...>>>` return type.
    ///  this argument allow adding extra auto traits bounds to the returned future.
    ///  Example: `async_extra_bounds(Send + 'static)`
    pub async_extra_bounds: Option<Punctuated<syn::TypeParamBound, Token![+]>>,
    /// Add tokens as a prefix for unique id.
    /// Usefull to differentiate multiple split functions with conflict module_name/fn_name.
    pub uniq_id_prefix: Option<Ident>,

    /// The path to the custom loader.
    /// In format of `custom_loader(some::path::to::loader::function)`, this function should be async and have signature:
    /// `async fn loader(module_id: ModuleId) -> bool`
    pub custom_loader: Option<Path>,

    /// The url to load the module from.
    /// Can be relative or absolute.
    pub with_url: Option<syn::LitStr>,

    /// Where to find the crate root for `wamex`.
    /// Usefull for reexporting `wamex` under different path.
    pub crate_path: Option<Path>,

    /// Add a side effect statement to be executed before loading the module.
    /// Usefull for logging or interacting with reactive systems.
    /// Example: `side_effect(log::info!("Loading module..."))`
    pub side_effect: Option<syn::Stmt>,
}

impl Parse for SplitArgs {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        macro_rules! ensure_is_empty {
            ($arg_name:expr) => {
                if let Some(arg_name) = $arg_name {
                    return Err(syn::Error::new(
                        arg_name.span(),
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
        let mut with_url: Option<syn::LitStr> = None;
        let mut crate_path: Option<Path> = None;
        let mut side_effect: Option<syn::Stmt> = None;
        let mut async_extra_bounds: Option<Punctuated<syn::TypeParamBound, Token![+]>> = None;

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
                        "with_url" => {
                            ensure_is_empty!(with_url);
                            with_url = Some(syn::parse2(tts)?);
                        }
                        "crate_path" => {
                            ensure_is_empty!(crate_path);
                            crate_path = Some(syn::parse2(tts)?);
                        }
                        "side_effect" => {
                            ensure_is_empty!(side_effect);
                            side_effect = Some(syn::parse2(tts)?);
                        }
                        "async_extra_bounds" => {
                            ensure_is_empty!(async_extra_bounds);
                            let bounds: Punctuated<syn::TypeParamBound, Token![+]> =
                                Punctuated::parse_terminated.parse2(tts)?;
                            async_extra_bounds = Some(bounds);
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
            with_url,
            crate_path,
            side_effect,
            async_extra_bounds,
        })
    }
}

/// Mark a function to be split into a separate WebAssembly module.
///
/// For more details on arguments usage see [`SplitArgs`] - it can be not shown in default docs building, so use `cargo doc --document-private-items`.
///
/// # Examples
/// ```ignore
/// use wamex::split;
/// #[split(my_module, wrap_return_with(SomeWrapper))]
/// async fn my_function(arg1: i32, arg2: String) -> ResultType {
///     // function body
/// }
/// ```
/// Using it in tandem with `wamex-cli` split command will extract this function into a separate WebAssembly module named `my_module_<VERSION>.wasm`,
/// and the function will be loaded and executed at runtime using the specified loader.
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
        crate_path,
        side_effect,
        async_extra_bounds,
    } = args;

    let vis = item_fn.vis;
    let name = &item_fn.sig.ident;

    let root = if let Some(crate_path) = crate_path {
        crate_path
    } else {
        parse_quote! { ::wamex }
    };
    let uniq_id_prefix = if let Some(uniq_id_prefix) = uniq_id_prefix {
        uniq_id_prefix
    } else {
        Ident::new("u", proc_macro2::Span::call_site())
    };
    // Unique identifier can help avoid name clashes when the same function is defined in multiple modules.
    // But using span for this make incremental extraction impossible - since a lot of symbols changes each time.
    // Instead of span we use file path. But currently this not fix a case where multiple functions have the same name in the same file.
    let unique_identifier =
        base16::encode_lower(&<Sha256 as Digest>::digest(format!("{name} {file_name}",))[..16]);

    let impl_import_ident = format_ident!(
        "__wamex_00{module_name}00_import_{name}_{uniq_id_prefix}{unique_identifier}"
    );
    let impl_export_ident = format_ident!(
        "__wamex_00{module_name}00_export_{name}_{uniq_id_prefix}{unique_identifier}"
    );

    let mut args = Vec::new();
    let mut args_types = Vec::new();
    for param in item_fn.sig.inputs.iter() {
        match param {
            syn::FnArg::Typed(pat_type) => {
                args.push(pat_type.pat.clone());
                args_types.push(pat_type.ty.clone());
            }
            syn::FnArg::Receiver(_) => {
                panic!("Methods with self parameter are not supported in #[split] macro");
            }
        }
    }
    let wamex_arg = format_ident!("__wamex_args");
    let pass_arg: Punctuated<FnArg, Token![,]> = parse_quote!( #wamex_arg: (#(#args_types),*) );

    let mut import_sig = Signature {
        ident: impl_import_ident.clone(),
        asyncness: None,
        inputs: pass_arg.clone(),
        ..item_fn.sig.clone()
    };
    let mut export_sig = Signature {
        ident: impl_export_ident.clone(),
        asyncness: None,
        inputs: pass_arg,
        ..item_fn.sig.clone()
    };

    let original_body_stmts = &item_fn.block.stmts;
    let mut body: TokenStream = if let Some(wrapper) = wrap_return_with {
        quote! {
            {
                let __wamex_result =  {
                    #(#original_body_stmts)*
                };
                #wrapper ( __wamex_result )
            }
        }
    } else {
        quote! { #(#original_body_stmts)* }
    };

    let ty = match &item_fn.sig.output {
        ReturnType::Default => quote! { () },
        ReturnType::Type(_, ty) => quote! { #ty },
    };

    let async_extra_bounds = if let Some(bounds) = async_extra_bounds {
        quote! { + #bounds }
    } else {
        quote! {}
    };

    let pin_box_ty: syn::Type = parse_quote! { ::core::pin::Pin<Box<dyn ::core::future::Future<Output = #ty> #async_extra_bounds>> };

    let is_async = item_fn.sig.asyncness.is_some();
    // Convert async fn to fn returning Pin<Box<dyn Future>>
    if is_async {
        let async_output: ReturnType = parse_quote! {
            -> #pin_box_ty
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

    let args_tuple = quote!((#(#args_types),*));
    let wrapper_output: ReturnType = if is_async {
        parse_quote!( -> ::wamex:: WamexLoadRunner<
                    #args_tuple, //args
                    impl ::core::future::Future<Output = bool>, //loader
                    ::wamex::UnsafeFn<#args_tuple,
                    #pin_box_ty
                    >,  #pin_box_ty>)
    } else {
        parse_quote!( -> ::wamex:: WamexLoadRunner<
            #args_tuple, //args
            impl ::core::future::Future<Output = bool>, //loader
            ::wamex::UnsafeFn<#args_tuple,
            #ty
            >,  ::wamex::NonAsync>)
    };

    let mut wrapper_sig = item_fn.sig;
    wrapper_sig.output = wrapper_output;
    wrapper_sig.asyncness = None;

    let attrs = item_fn.attrs;

    let loader = if let Some(custom_loader) = custom_loader {
        quote! { #custom_loader }
    } else {
        quote! { #root::load }
    };

    let module_name_str = module_name.to_string();

    let module_id = if let Some(url) = with_url {
        quote! {
            #root::ModuleId::with_url(#module_name_str, #url)
        }
    } else {
        quote! {
            #root::ModuleId::new(#module_name_str)
        }
    };

    let side_effect = if let Some(side_effect) = side_effect {
        quote! {
            #side_effect
        }
    } else {
        quote! {}
    };

    let load_and_execute = if is_async {
        quote! { #root::load_and_execute }
    } else {
        quote! { #root::load_and_execute_sync }
    };

    quote! {
        #(#attrs)*
        #vis #wrapper_sig {

            // This import will be replaced so we can place any module name here
            #[link(wasm_import_module = "./__wamex_loader.rs")]
            extern "C" {

                #[allow(improper_ctypes)]
                #import_sig;
            }

            #[allow(improper_ctypes_definitions)]
            #[unsafe(no_mangle)]
            pub extern "C" #export_sig {
                let (#(#args)*) : ( #(#args_types),* ) = #wamex_arg;
                #body
            }

            #side_effect
            #load_and_execute(
                #loader(#module_id),
                #impl_import_ident,
                ( #(#args),* ))

        }
    }
}
