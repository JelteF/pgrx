/*
Portions Copyright 2019-2021 ZomboDB, LLC.
Portions Copyright 2021-2022 Technology Concepts & Design, Inc. <support@tcdi.com>

All rights reserved.

Use of this source code is governed by the MIT license that can be found in the LICENSE file.
*/
use crate::{anonymonize_lifetimes, anonymonize_lifetimes_in_type_path};
use eyre::eyre;
use proc_macro2::{Ident, Span, TokenStream as TokenStream2};
use quote::{quote, ToTokens, TokenStreamExt};
use std::convert::TryFrom;
use syn::{
    parse::{Parse, ParseStream},
    Token,
};

#[derive(Debug, Clone)]
pub struct ReturningIteratedItem {
    ty: syn::Type,
    name: Option<String>,
    sql: Option<syn::Expr>
}

#[derive(Debug, Clone)]
pub enum Returning {
    None,
    Type { ty: syn::Type, sql: Option<syn::Expr>, },
    SetOf { ty: syn::TypePath, sql: Option<syn::Expr>, },
    Iterated(Vec<ReturningIteratedItem>),
    /// `pgx_pg_sys::Datum`
    Trigger,
}

impl Returning {
    fn parse_trait_bound(trait_bound: &mut syn::TraitBound) -> Returning {
        let last_path_segment = trait_bound.path.segments.last_mut().unwrap();
        match last_path_segment.ident.to_string().as_str() {
            "Iterator" => match &mut last_path_segment.arguments {
                syn::PathArguments::AngleBracketed(args) => match args.args.first_mut().unwrap() {
                    syn::GenericArgument::Binding(binding) => match &mut binding.ty {
                        syn::Type::Tuple(tuple_type) => Self::parse_type_tuple(tuple_type),
                        syn::Type::Path(path) => {
                            Returning::SetOf { ty: anonymonize_lifetimes_in_type_path(path.clone()), sql: None }
                        }
                        syn::Type::Reference(type_ref) => match &*type_ref.elem {
                            syn::Type::Path(path) => {
                                Returning::SetOf { ty: anonymonize_lifetimes_in_type_path(path.clone()), sql: None }
                            }
                            _ => unimplemented!("Expected path"),
                        },
                        ty => unimplemented!("Only iters with tuples, got {:?}.", ty),
                    },
                    _ => unimplemented!(),
                },
                _ => unimplemented!(),
            },
            _ => unimplemented!(),
        }
    }

    fn parse_type_tuple(type_tuple: &mut syn::TypeTuple) -> Returning {
        let returns: Vec<ReturningIteratedItem> = type_tuple
            .elems
            .iter_mut()
            .flat_map(|elem| {
                let mut elem = elem.clone();
                anonymonize_lifetimes(&mut elem);

                match elem {
                    syn::Type::Macro(macro_pat) => {
                        // This is essentially a copy of `parse_type_macro` but it returns items instead of `Returning`
                        let mac = &macro_pat.mac;
                        let archetype = mac.path.segments.last().unwrap();
                        match archetype.ident.to_string().as_str() {
                            "name" => {
                                let out: NameMacro = mac
                                    .parse_body()
                                    .expect(&*format!("Failed to parse named!(): {:?}", mac));
                                Some(ReturningIteratedItem { ty: out.ty, name: Some(out.ident), sql: out.sql })
                            },
                            "composite_type" => {
                                let sql: syn::Expr = mac.parse_body().expect(&*format!("Failed to parse composite_type!(): {:?}", mac));
                                Some(ReturningIteratedItem {
                                    ty: syn::parse_quote! { ::pgx::PgHeapTuple<'_, impl WhoAllocated<::pgx::pg_sys::HeapTupleData>> },
                                    name: None,
                                    sql: Some(sql),
                                })
                            }
                            _ => unimplemented!("Don't support anything other than `name!()` and `composite_type!()`"),
                        }
                    },
                    ty => Some(ReturningIteratedItem { ty: ty.clone(), name: None, sql: None }),
                }
            })
            .collect();
        Returning::Iterated(returns)
    }

    fn parse_impl_trait(impl_trait: &mut syn::TypeImplTrait) -> Returning {
        match impl_trait.bounds.first_mut().unwrap() {
            syn::TypeParamBound::Trait(trait_bound) => Self::parse_trait_bound(trait_bound),
            _ => Returning::None,
        }
    }

    fn parse_type_macro(type_macro: &mut syn::TypeMacro) -> Returning {
        let mac = &type_macro.mac;
        let archetype = mac.path.segments.last().unwrap();
        match archetype.ident.to_string().as_str() {
            "composite_type" => {
                let sql: syn::Expr = mac.parse_body().expect(&*format!("Failed to parse composite_type!(): {:?}", mac));
                Returning::Type {
                    ty: syn::parse_quote! { ::pgx::PgHeapTuple<'_, impl WhoAllocated<::pgx::pg_sys::HeapTupleData>> },
                    sql: Some(sql),
                }
            }
            _ => unimplemented!("Don't support anything other than `composite_type!()`"),
        }
    }

    fn parse_dyn_trait(dyn_trait: &mut syn::TypeTraitObject) -> Returning {
        match dyn_trait.bounds.first_mut().unwrap() {
            syn::TypeParamBound::Trait(trait_bound) => Self::parse_trait_bound(trait_bound),
            _ => Returning::None,
        }
    }
}

impl TryFrom<&syn::ReturnType> for Returning {
    type Error = eyre::Error;

    fn try_from(value: &syn::ReturnType) -> Result<Self, Self::Error> {
        Ok(match &value {
            syn::ReturnType::Default => Returning::None,
            syn::ReturnType::Type(_, ty) => {
                let mut ty = *ty.clone();
                anonymonize_lifetimes(&mut ty);

                match ty {
                    syn::Type::ImplTrait(mut impl_trait) => {
                        Returning::parse_impl_trait(&mut impl_trait)
                    },
                    syn::Type::TraitObject(mut dyn_trait) => {
                        Returning::parse_dyn_trait(&mut dyn_trait)
                    },
                    syn::Type::Path(mut typepath) => {
                        let path = &mut typepath.path;
                        let mut saw_pg_sys = false;
                        let mut saw_datum = false;
                        let mut saw_option_ident = false;
                        let mut saw_box_ident = false;
                        let mut maybe_inner_impl_trait = None;

                        for segment in &mut path.segments {
                            let ident_string = segment.ident.to_string();
                            match ident_string.as_str() {
                                "pg_sys" => saw_pg_sys = true,
                                "Datum" => saw_datum = true,
                                "Option" => saw_option_ident = true,
                                "Box" => saw_box_ident = true,
                                _ => (),
                            }
                            if saw_option_ident || saw_box_ident {
                                match &mut segment.arguments {
                                    syn::PathArguments::AngleBracketed(inside_brackets) => {
                                        match inside_brackets.args.first_mut() {
                                            Some(syn::GenericArgument::Type(
                                                syn::Type::ImplTrait(impl_trait),
                                            )) => {
                                                maybe_inner_impl_trait =
                                                    Some(Returning::parse_impl_trait(impl_trait));
                                            },
                                            Some(syn::GenericArgument::Type(
                                                syn::Type::TraitObject(dyn_trait),
                                            )) => {
                                                maybe_inner_impl_trait =
                                                    Some(Returning::parse_dyn_trait(dyn_trait))
                                            },
                                            _ => (),
                                        }
                                    }
                                    syn::PathArguments::None
                                    | syn::PathArguments::Parenthesized(_) => (),
                                }
                            }
                        }
                        if (saw_datum && saw_pg_sys) || (saw_datum && path.segments.len() == 1) {
                            Returning::Trigger
                        } else if let Some(returning) = maybe_inner_impl_trait {
                            returning
                        } else {
                            let mut static_ty = typepath.clone();
                            for segment in &mut static_ty.path.segments {
                                match &mut segment.arguments {
                                    syn::PathArguments::AngleBracketed(ref mut inside_brackets) => {
                                        for mut arg in &mut inside_brackets.args {
                                            match &mut arg {
                                                syn::GenericArgument::Lifetime(
                                                    ref mut lifetime,
                                                ) => {
                                                    lifetime.ident =
                                                        Ident::new("static", Span::call_site())
                                                }
                                                _ => (),
                                            }
                                        }
                                    },
                                    _ => (),
                                }
                            }
                            Returning::Type { ty: syn::Type::Path(static_ty.clone()), sql: None }
                        }
                    },
                    syn::Type::Reference(mut ty_ref) => {
                        if let Some(ref mut lifetime) = &mut ty_ref.lifetime {
                            lifetime.ident = Ident::new("static", Span::call_site());
                        }
                        Returning::Type { ty: syn::Type::Reference(ty_ref), sql: None } 
                    },
                    syn::Type::Tuple(ref mut tup) => {
                        if tup.elems.is_empty() {
                            Returning::Type { ty: ty.clone(), sql: None }
                        } else {
                            Self::parse_type_tuple(tup)
                        }
                    },
                    syn::Type::Macro(ref mut type_macro) => {
                        Self::parse_type_macro(type_macro)
                    },
                    _ => return Err(eyre!("Got unknown return type: {}", &ty.to_token_stream())),
                }
            }
        })
    }
}

impl ToTokens for Returning {
    fn to_tokens(&self, tokens: &mut TokenStream2) {
        let quoted = match self {
            Returning::None => quote! {
                ::pgx::utils::sql_entity_graph::PgExternReturnEntity::None
            },
            Returning::Type { ty, sql} => {
                if let Some(sql) = sql {
                    quote! {
                        ::pgx::utils::sql_entity_graph::PgExternReturnEntity::Type {
                            ty: ::pgx::utils::sql_entity_graph::TypeEntity::CompositeType {
                                sql: #sql,
                            }
                        }
                    }
                } else {
                    let ty_string = ty.to_token_stream().to_string().replace(" ", "");
                    let sql_iter = sql.iter();
                    quote! {
                        ::pgx::utils::sql_entity_graph::PgExternReturnEntity::Type {
                            ty: ::pgx::utils::sql_entity_graph::TypeEntity::Type {
                                ty_id: TypeId::of::<#ty>(),
                                ty_source: #ty_string,
                                full_path: core::any::type_name::<#ty>(),
                                module_path: {
                                    let type_name = core::any::type_name::<#ty>();
                                    let mut path_items: Vec<_> = type_name.split("::").collect();
                                    let _ = path_items.pop(); // Drop the one we don't want.
                                    path_items.join("::")
                                },
                            }
                        }
                    }
                }
                
            }
            Returning::SetOf { ty, sql } => {
                if let Some(sql) = sql {
                    quote! {
                        ::pgx::utils::sql_entity_graph::PgExternReturnEntity::SetOf {
                            ty: ::pgx::utils::sql_entity_graph::TypeEntity::CompositeType {
                                sql: #sql,
                            }
                        }
                    }
                } else {
                    let ty_string = ty.to_token_stream().to_string().replace(" ", "");
                    quote! {
                        ::pgx::utils::sql_entity_graph::PgExternReturnEntity::SetOf {
                            ty: ::pgx::utils::sql_entity_graph::TypeEntity::Type {
                                ty_id: TypeId::of::<#ty>(),
                                ty_source: #ty_string,
                                full_path: core::any::type_name::<#ty>(),
                                module_path: {
                                    let type_name = core::any::type_name::<#ty>();
                                    let mut path_items: Vec<_> = type_name.split("::").collect();
                                    let _ = path_items.pop(); // Drop the one we don't want.
                                    path_items.join("::")
                                },
                            },
                        }
                    }
                }
            }
            Returning::Iterated(items) => {
                let quoted_items = items
                    .iter()
                    .map(|ReturningIteratedItem { ty, name, sql }| {
                        let ty_string = ty.to_token_stream().to_string().replace(" ", "");
                        let name_iter = name.iter();
                        quote! {
                            (
                                ::pgx::utils::sql_entity_graph::TypeEntity::Type {
                                    ty_id: TypeId::of::<#ty>(),
                                    ty_source: #ty_string,
                                    full_path: core::any::type_name::<#ty>(),
                                    module_path: {
                                        let type_name = core::any::type_name::<#ty>();
                                        let mut path_items: Vec<_> = type_name.split("::").collect();
                                        let _ = path_items.pop(); // Drop the one we don't want.
                                        path_items.join("::")
                                    },
                                },
                                None #( .unwrap_or(Some(stringify!(#name_iter))) )*,
                            )
                        }
                    })
                    .collect::<Vec<_>>();
                quote! {
                    ::pgx::utils::sql_entity_graph::PgExternReturnEntity::Iterated(vec![
                        #(#quoted_items),*
                    ])
                }
            }
            Returning::Trigger => quote! {
                ::pgx::utils::sql_entity_graph::PgExternReturnEntity::Trigger
            },
        };
        tokens.append_all(quoted);
    }
}

#[derive(Debug, Clone)]
pub struct NameMacro {
    pub(crate) ident: String,
    pub(crate) ty: syn::Type,
    pub(crate) sql: Option<syn::Expr>,
}

impl Parse for NameMacro {
    fn parse(input: ParseStream) -> Result<Self, syn::Error> {
        let ident = input
            .parse::<syn::Ident>()
            .map(|v| v.to_string())
            // Avoid making folks unable to use rust keywords.
            .or_else(|_| {
                input
                    .parse::<syn::Token![type]>()
                    .map(|_| String::from("type"))
            })
            .or_else(|_| {
                input
                    .parse::<syn::Token![mod]>()
                    .map(|_| String::from("mod"))
            })
            .or_else(|_| {
                input
                    .parse::<syn::Token![extern]>()
                    .map(|_| String::from("extern"))
            })
            .or_else(|_| {
                input
                    .parse::<syn::Token![async]>()
                    .map(|_| String::from("async"))
            })
            .or_else(|_| {
                input
                    .parse::<syn::Token![crate]>()
                    .map(|_| String::from("crate"))
            })
            .or_else(|_| {
                input
                    .parse::<syn::Token![use]>()
                    .map(|_| String::from("use"))
            })?;
        let _comma: Token![,] = input.parse()?;
        let ty = input.parse()?;

        
        let sql = match &ty {
            syn::Type::Macro(ref macro_pat) => {
                // This is essentially a copy of `parse_type_macro` but it returns items instead of `Returning`
                let mac = &macro_pat.mac;
                let archetype = mac.path.segments.last().unwrap();
                match archetype.ident.to_string().as_str() {
                    "composite_type" => {
                        Some(mac.parse_body().expect(&*format!("Failed to parse composite_type!(): {:?}", mac)))
                    }
                    _ => unimplemented!("Don't support anything other than `name!()` and `composite_type!()`"),
                }
            },
            _ => None,
        };

        Ok(Self { ident, ty, sql })
    }
}
