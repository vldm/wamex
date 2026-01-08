//! Derive macro to implement Constraints trait for relocation entry generics.
//! Useless outside of wamex and done as experiment to implement compile-time constraints on enum variants.

use proc_macro::TokenStream;
use proc_macro2::{TokenStream as TokenStream2, TokenTree as TokenTree2};
use quote::{format_ident, quote};
use syn::{Attribute, Data};

#[proc_macro_derive(Constraints, attributes(has, index_type))]
pub fn derive_constraints(enum_decl: TokenStream) -> TokenStream {
    match derive_constraints2(enum_decl.into()) {
        Ok(ts) => ts.into(),
        Err(err) => err.into_compile_error().into(),
    }
}
#[derive(Default)]
struct Constraints {
    tls: bool,
    base: bool,
    has64: bool,
    int: bool,
    leb: bool,
    sleb: bool,
    addend: bool,
    index_type: Option<TokenStream2>,
}
fn take_constrains_attribute(attributes: Vec<Attribute>) -> syn::Result<Constraints> {
    let mut constraints = Constraints::default();

    for attr in attributes {
        let Ok(list) = attr.meta.require_list() else {
            continue;
        };
        let attr_name = list.path.require_ident()?;
        if attr_name == "has" {
            let mut iter = list.tokens.clone().into_iter().peekable();
            while let Some(TokenTree2::Ident(ident)) = iter.peek() {
                match ident.to_string().as_str() {
                    "tls" => constraints.tls = true,
                    "base" => constraints.base = true,
                    "has64" => constraints.has64 = true,
                    "int" => constraints.int = true,
                    "leb" => constraints.leb = true,
                    "sleb" => constraints.sleb = true,
                    "addend" => constraints.addend = true,
                    _ => break,
                }
                iter.next(); // consume token
                let Some(TokenTree2::Punct(punct)) = iter.next() else {
                    break;
                };
                if punct.as_char() != ',' {
                    break;
                }
            }

            if let Some(token) = iter.next() {
                return Err(syn::Error::new_spanned(token, "Unexpected token"));
            }
        } else if attr_name == "index_type" {
            constraints.index_type = Some(list.tokens.clone());
        }
    }
    Ok(constraints)
}

// Expect enum variant in form:
// enum Foo {
//   #[has(...)]
//   Var1(SomeGeneric<Generic>),
//   ..
// }
struct EnumVariant {
    name: syn::Ident,
    common_type: syn::Path,
    _lt_token: syn::Token![<],
    generic: syn::Path,
    _gt_token: syn::Token![>],
}

fn parse_variant(variant: &syn::Variant) -> syn::Result<EnumVariant> {
    let name = variant.ident.clone();
    let syn::Fields::Unnamed(unnamed_fields) = &variant.fields else {
        return Err(syn::Error::new_spanned(&variant, "Expected unnamed fields"));
    };
    if unnamed_fields.unnamed.len() != 1 {
        return Err(syn::Error::new_spanned(
            &variant.fields,
            "Expected single field in variant",
        ));
    }

    let field = unnamed_fields.unnamed.first().unwrap();
    // split field_ty into ident and generic
    let field_full_type = &field.ty;
    let syn::Type::Path(type_path) = field_full_type else {
        return Err(syn::Error::new_spanned(
            field_full_type,
            "Expected type path",
        ));
    };
    // extract generic from path
    let segments = type_path.path.segments.clone();
    if segments.len() < 1 {
        return Err(syn::Error::new_spanned(
            type_path,
            "Expected path with generic",
        ));
    }

    let last_segment = segments.last().unwrap();
    let syn::PathArguments::AngleBracketed(angle_bracketed) = &last_segment.arguments else {
        return Err(syn::Error::new_spanned(
            &last_segment.arguments,
            "Expected angle bracketed arguments",
        ));
    };
    if angle_bracketed.args.len() != 1 {
        return Err(syn::Error::new_spanned(
            angle_bracketed,
            "Expected single generic argument",
        ));
    }
    let generic_arg = angle_bracketed.args.first().unwrap();
    let syn::GenericArgument::Type(syn::Type::Path(generic_type_path)) = generic_arg else {
        return Err(syn::Error::new_spanned(
            generic_arg,
            "Expected type generic argument",
        ));
    };

    let mut common_type = type_path.path.clone();
    common_type.segments.pop();
    common_type.segments.push(syn::PathSegment {
        ident: last_segment.ident.clone(),
        arguments: syn::PathArguments::None,
    });

    Ok(EnumVariant {
        name,
        common_type,
        _lt_token: angle_bracketed.lt_token,
        generic: generic_type_path.path.clone(),
        _gt_token: angle_bracketed.gt_token,
    })
}

fn derive_constraints2(enum_decl: TokenStream2) -> syn::Result<TokenStream2> {
    let ast: syn::DeriveInput = syn::parse2(enum_decl).unwrap();

    let name = &ast.ident;

    let Data::Enum(v) = ast.data else {
        return Err(syn::Error::new_spanned(
            &ast,
            "Derive Constraints should be used only for enum",
        ));
    };

    let mut variants = Vec::new();
    for variant in v.variants.iter() {
        let constraints = take_constrains_attribute(variant.attrs.clone())?;
        let variant = parse_variant(variant)?;
        variants.push((variant, constraints));
    }

    let mut result = TokenStream2::new();
    result.extend(impl_constraints(&variants));
    result.extend(impl_from(name, &variants));

    Ok(result)
}

// impl of
//  impl Constraints for $idx {
//     type RelTls = constraints!(@has RelTls ; $($cap),*);
//     type RelBase = constraints!(@has RelBase ; $($cap),*);
//     type Has64 = constraints!(@has Has64 ; $($cap),*);
//     type Int = constraints!(@has Int ; $($cap),*);
//     type Sleb = constraints!(@has Sleb ; $($cap),*);
//     type Leb = constraints!(@has Leb ; $($cap),*);
//     type Addend = constraints!(@has Addend ; $($cap),*);
//     type IndexType = constraints!(@index $($index_type)?);
// }
// for extracted generic from variant
fn impl_constraints(variants: &[(EnumVariant, Constraints)]) -> TokenStream2 {
    let mut result = TokenStream2::new();
    for (variant, constraints) in variants.iter() {
        let EnumVariant { generic, .. } = variant;

        let rel_tls = to_types(constraints.tls);
        let rel_base = to_types(constraints.base);
        let has64 = to_types(constraints.has64);
        let int = to_types(constraints.int);
        let sleb = to_types(constraints.sleb);
        let leb = to_types(constraints.leb);
        let addend = to_types(constraints.addend);
        let index_type = constraints
            .index_type
            .clone()
            .unwrap_or_else(|| quote!(SymbolId));

        result.extend(quote! {
             impl Constraints for #generic {
                type RelTls = #rel_tls;
                type RelBase = #rel_base;
                type Has64 = #has64;
                type Int = #int;
                type Sleb = #sleb;
                type Leb = #leb;
                type Addend = #addend;
                type IndexType = #index_type;
            }
        });
    }
    result
}

fn impl_from(name: &syn::Ident, variants: &[(EnumVariant, Constraints)]) -> TokenStream2 {
    let mut from_branchs = vec![];

    let mut to_branches = vec![];

    for (variant, constraints) in variants {
        let products = constraints_product(constraints);
        for product in products {
            let match_name = build_original_tag(&variant.name, product);

            let build_name = &variant.name;
            let type_name = &variant.common_type;

            let width_builder = match product.2 {
                Width::W32 => quote!(RelocationWidth::Bits32(Has)),
                Width::W64 => quote!(RelocationWidth::Bits64(Has)),
            };
            let encoding_builder = match product.1 {
                Encoding::Fixed => quote!(Encoding::Fixed(Has)),
                Encoding::Sleb => quote!(Encoding::Sleb(Has)),
                Encoding::Leb => quote!(Encoding::Leb(Has)),
            };
            let relation_builder = match product.0 {
                Rel::Got => quote!(Relative::Got(Has)),
                Rel::Tls => quote!(Relative::Tls(Has)),
                Rel::None => quote!(Relative::None(Has)),
            };
            let addend_builder = if constraints.addend {
                quote!(Addend::some(value.addend))
            } else {
                quote!(Addend::none())
            };

            from_branchs.push(quote! {
                wasmparser::RelocationType::#match_name => {
                    Self::#build_name(
                        #type_name {
                            addend: #addend_builder,
                            relation: #relation_builder,
                            encoding: #encoding_builder,
                            width: #width_builder,
                            index: EntityRef::new(value.index as usize),
                            offset: value.offset - symbol_start,
                            index_type: PhantomData,
                        }
                    )
                }
            });

            to_branches.push(quote! {
                Self::#build_name(inner) => {
                    wasmparser::RelocationEntry {
                        ty: wasmparser::RelocationType::#match_name,
                        offset: inner.offset + symbol_start,
                        index: inner.index.as_u32(),
                        addend: inner.addend.to_option().unwrap_or_default(),
                    }

                }
            });
        }
    }

    quote! {
        impl #name {
            /// Converts from raw wasmparser relocation entry to typed relocation entry.
            /// shifting offset by symbol_start to be relative to symbol start.
            pub fn from_raw(
                value: wasmparser::RelocationEntry,
                symbol_start: u32,) -> Self {
                match value.ty {
                    #(#from_branchs),*,
                }
            }

            /// Converts from typed relocation entry to raw wasmparser relocation entry.
            /// shifting offset back to be relative to section start.
            pub fn into_raw(
                self,
                symbol_start: u32,) -> wasmparser::RelocationEntry {
                match self {
                    #(#to_branches),*,
                }
            }
        }
    }
}

#[derive(Clone, Copy)]
enum Encoding {
    Fixed,
    Sleb,
    Leb,
}
#[derive(Clone, Copy)]
enum Width {
    W32,
    W64,
}
#[derive(Clone, Copy)]
enum Rel {
    None,
    Got,
    Tls,
}
type ConstraintVariant = (Rel, Encoding, Width);
// return all valid variants of components
// (base, encoding, width)
fn constraints_product(constraints: &Constraints) -> Vec<ConstraintVariant> {
    let mut encodings = vec![];

    if constraints.leb {
        encodings.push(Encoding::Leb);
    }
    if constraints.sleb {
        encodings.push(Encoding::Sleb);
    }
    if constraints.int {
        encodings.push(Encoding::Fixed)
    }

    let mut result: Vec<_> = encodings
        .iter()
        .copied()
        .map(|e| (Rel::None, e, Width::W32))
        .collect();
    if constraints.has64 {
        result.extend(
            encodings
                .iter()
                .copied()
                .map(|e| (Rel::None, e, Width::W64)),
        );
    }

    if constraints.base {
        result.push((Rel::Got, Encoding::Sleb, Width::W32));
        if constraints.has64 {
            result.push((Rel::Got, Encoding::Sleb, Width::W64));
        }
    }

    if constraints.tls {
        result.push((Rel::Tls, Encoding::Sleb, Width::W32));
        if constraints.has64 {
            result.push((Rel::Tls, Encoding::Sleb, Width::W64));
        }
    }

    result
}

fn build_original_tag(
    variant_name: &syn::Ident,
    (rel, encoding, width): ConstraintVariant,
) -> syn::Ident {
    let text_encoding = match (encoding, width) {
        (Encoding::Fixed, Width::W32) => "I32",
        (Encoding::Fixed, Width::W64) => "I64",
        (Encoding::Sleb, Width::W32) => "Sleb",
        (Encoding::Sleb, Width::W64) => "Sleb64",
        (Encoding::Leb, Width::W32) => "Leb",
        (Encoding::Leb, Width::W64) => "Leb64",
    };
    let rel_text = match rel {
        Rel::None => "",
        Rel::Tls => "Tls",
        Rel::Got => "Rel",
    };

    format_ident!(
        "{variant_name}{rel_text}{text_encoding}",
        span = variant_name.span()
    )
}

fn to_types(v: bool) -> TokenStream2 {
    if v {
        quote! {impl_type_safety::Yes}
    } else {
        quote! {impl_type_safety::No}
    }
}
