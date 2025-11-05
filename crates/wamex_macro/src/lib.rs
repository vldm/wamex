use digest::Digest;
use proc_macro::TokenStream;
use quote::{format_ident, quote};
use syn::{
    parse::{Parse, ParseStream},
    parse_macro_input, parse_quote, Ident, ItemFn, ReturnType, Signature,
};

struct SplitArgs {
    module_name: Ident,
}

impl Parse for SplitArgs {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let module_name = input.parse()?;
        Ok(Self { module_name })
    }
}

#[proc_macro_attribute]
pub fn wasm_split(args: TokenStream, input: TokenStream) -> TokenStream {
    let SplitArgs { module_name } = parse_macro_input!(args as SplitArgs);
    let item_fn = parse_macro_input!(input as ItemFn);

    let vis = item_fn.vis;

    let name = &item_fn.sig.ident;

    // Unique identifier can help avoid name clashes when the same function is defined in multiple modules.
    // But using span for this make incremental extraction impossible - since a lot of symbols changes each time.
    // TODO: Instead of span we can use file path. But currently we don't have access to it here, so just force user to provide unique function names.
    let unique_identifier = base16::encode_lower(&sha2::Sha256::digest(format!("{name}",))[..16]);

    let impl_import_ident =
        format_ident!("__wasm_split_00{module_name}00_import_{unique_identifier}_{name}");
    let impl_export_ident =
        format_ident!("__wasm_split_00{module_name}00_export_{unique_identifier}_{name}");

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

    let stmts = &item_fn.block.stmts;
    let is_async = item_fn.sig.asyncness.is_some();
    // Convert async fn to fn returning Pin<Box<dyn Future>>
    let body = if is_async {
        let ty = match &item_fn.sig.output {
            ReturnType::Default => quote! { () },
            ReturnType::Type(_, ty) => quote! { #ty },
        };
        let async_output: ReturnType = parse_quote! {
            -> ::core::pin::Pin<Box<dyn ::core::future::Future<Output = #ty>>>
        };
        export_sig.output = async_output.clone();
        import_sig.output = async_output;

        quote! {
            Box::pin(async move {
                #(#stmts)*
            })
        }
    } else {
        quote! { #(#stmts)* }
    };

    let mut wrapper_sig = item_fn.sig;
    wrapper_sig.asyncness = Some(Default::default());
    let mut args = Vec::new();
    for (i, param) in wrapper_sig.inputs.iter_mut().enumerate() {
        match param {
            syn::FnArg::Typed(pat_type) => {
                let param_ident = format_ident!("__wasm_split_arg_{i}");
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
            let id = ::wamex::ModuleId::new(stringify!(#module_name));

            ::wamex::load(id.clone(), false).await.unwrap();

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
