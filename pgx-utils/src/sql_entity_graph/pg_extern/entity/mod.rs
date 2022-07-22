/*
Portions Copyright 2019-2021 ZomboDB, LLC.
Portions Copyright 2021-2022 Technology Concepts & Design, Inc. <support@tcdi.com>

All rights reserved.

Use of this source code is governed by the MIT license that can be found in the LICENSE file.
*/
mod argument;
mod operator;
mod returning;

pub use argument::PgExternArgumentEntity;
pub use operator::PgOperatorEntity;
pub use returning::{PgExternReturnEntity, PgExternReturnEntityIteratedItem};

use crate::{
    sql_entity_graph::{
        metadata::SqlVariant,
        pgx_sql::PgxSql,
        to_sql::{entity::ToSqlConfigEntity, ToSql},
        SqlGraphEntity, SqlGraphIdentifier,
    },
    ExternArgs,
};

use eyre::{eyre, WrapErr};
use std::cmp::Ordering;

/// The output of a [`PgExtern`](crate::sql_entity_graph::pg_extern::PgExtern) from `quote::ToTokens::to_tokens`.
#[derive(Debug, Clone)]
pub struct PgExternEntity {
    pub name: &'static str,
    pub unaliased_name: &'static str,
    pub module_path: &'static str,
    pub full_path: &'static str,
    pub metadata: crate::sql_entity_graph::metadata::FunctionMetadataEntity,
    pub fn_args: Vec<PgExternArgumentEntity>,
    pub fn_return: PgExternReturnEntity,
    pub schema: Option<&'static str>,
    pub file: &'static str,
    pub line: u32,
    pub extern_attrs: Vec<ExternArgs>,
    pub search_path: Option<Vec<&'static str>>,
    pub operator: Option<PgOperatorEntity>,
    pub to_sql_config: ToSqlConfigEntity,
}

impl std::hash::Hash for PgExternEntity {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.metadata.hash(state);
    }
}

impl PartialEq for PgExternEntity {
    fn eq(&self, other: &Self) -> bool {
        self.metadata.eq(&other.metadata)
    }
}

impl Eq for PgExternEntity {}

impl Ord for PgExternEntity {
    fn cmp(&self, other: &Self) -> Ordering {
        self.metadata.cmp(&other.metadata)
    }
}

impl PartialOrd for PgExternEntity {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Into<SqlGraphEntity> for PgExternEntity {
    fn into(self) -> SqlGraphEntity {
        SqlGraphEntity::Function(self)
    }
}

impl SqlGraphIdentifier for PgExternEntity {
    fn dot_identifier(&self) -> String {
        format!("fn {}", self.name)
    }
    fn rust_identifier(&self) -> String {
        self.metadata.path.to_string()
    }

    fn file(&self) -> Option<&'static str> {
        Some(self.file)
    }

    fn line(&self) -> Option<u32> {
        Some(self.line)
    }
}

impl ToSql for PgExternEntity {
    #[tracing::instrument(
        level = "error",
        skip(self, context),
        fields(identifier = %self.rust_identifier()),
    )]
    fn to_sql(&self, context: &PgxSql) -> eyre::Result<String> {
        let self_index = context.externs[self];
        let mut extern_attrs = self.extern_attrs.clone();
        // if we already have a STRICT marker we do not need to add it
        let mut strict_upgrade = !extern_attrs.iter().any(|i| i == &ExternArgs::Strict);
        if strict_upgrade {
            for arg in &self.metadata.arguments {
                if arg.optional || arg.type_id == context.internal_type {
                    strict_upgrade = false;
                }
            }
        }

        if strict_upgrade {
            extern_attrs.push(ExternArgs::Strict);
        }

        let module_pathname = &context.get_module_pathname();

        let fn_sql = format!(
            "\
                                CREATE FUNCTION {schema}\"{name}\"({arguments}) {returns}\n\
                                {extern_attrs}\
                                {search_path}\
                                LANGUAGE c /* Rust */\n\
                                AS '{module_pathname}', '{name}_wrapper';\
                            ",
            schema = self
                .schema
                .map(|schema| format!("{}.", schema))
                .unwrap_or_else(|| context.schema_prefix_for(&self_index)),
            name = self.name,
            module_pathname = module_pathname,
            arguments = if !self.metadata.arguments.is_empty() {
                let mut args = Vec::new();
                for (idx, arg) in self.metadata.arguments.iter().enumerate() {
                    let arg_pattern = self.fn_args[idx].pattern;
                    let arg_default = self.fn_args[idx].used_ty.default;
                    let graph_index = context
                        .graph
                        .neighbors_undirected(self_index)
                        .find(|neighbor| match &context.graph[*neighbor] {
                            SqlGraphEntity::Type(ty) => ty.id_matches(&arg.type_id),
                            SqlGraphEntity::Enum(en) => en.id_matches(&arg.type_id),
                            SqlGraphEntity::BuiltinType(defined) => defined == &arg.type_name,
                            _ => false,
                        })
                        .ok_or_else(|| eyre!("Could not find arg type in graph. Got: {:?}", arg))?;
                    let needs_comma = idx < (self.metadata.arguments.len() - 1);
                    match arg.argument_sql {
                        Ok(SqlVariant::Mapped(ref argument_sql)) => {
                            let buf = format!("\
                                                \t\"{pattern}\" {variadic}{schema_prefix}{sql_type}{default}{maybe_comma}/* {type_name} */\
                                            ",
                                                pattern = arg_pattern,
                                                schema_prefix = context.schema_prefix_for(&graph_index),
                                                // First try to match on [`TypeId`] since it's most reliable.
                                                sql_type = argument_sql,
                                                default = if let Some(def) = arg_default { format!(" DEFAULT {}", def) } else { String::from("") },
                                                variadic = if arg.variadic { "VARIADIC " } else { "" },
                                                maybe_comma = if needs_comma { ", " } else { " " },
                                                type_name = arg.type_name,
                                        );
                            args.push(buf);
                        }
                        Ok(SqlVariant::Composite {
                            requires_array_brackets,
                        }) => {
                            let sql = self.fn_args[idx]
                                .used_ty
                                .composite_type
                                .map(|v| {
                                    if requires_array_brackets {
                                        format!("{v}[]")
                                    } else {
                                        format!("{v}")
                                    }
                                })
                                .ok_or_else(|| {
                                    eyre!(
                                    "Macro expansion time suggested a composite_type!() in return"
                                )
                                })?;
                            let buf = format!("\
                                \t\"{pattern}\" {variadic}{schema_prefix}{sql_type}{default}{maybe_comma}/* {type_name} */\
                            ",
                                pattern = arg_pattern,
                                schema_prefix = context.schema_prefix_for(&graph_index),
                                // First try to match on [`TypeId`] since it's most reliable.
                                sql_type = sql,
                                default = if let Some(def) = arg_default { format!(" DEFAULT {}", def) } else { String::from("") },
                                variadic = if arg.variadic { "VARIADIC " } else { "" },
                                maybe_comma = if needs_comma { ", " } else { " " },
                                type_name = arg.type_name,
                        );
                            args.push(buf);
                        }
                        Ok(SqlVariant::Skip) => (),
                        Err(err) => return Err(err).wrap_err("While mapping argument"),
                    }
                }
                String::from("\n") + &args.join("\n") + "\n"
            } else {
                Default::default()
            },
            returns = match self.metadata.retval {
                None => String::from("RETURNS void"),
                Some(ref retval) => {
                    let graph_index = context
                        .graph
                        .neighbors_undirected(self_index)
                        .find(|neighbor| match &context.graph[*neighbor] {
                            SqlGraphEntity::Type(ty) => ty.id_matches(&retval.type_id),
                            SqlGraphEntity::Enum(en) => en.id_matches(&retval.type_id),
                            SqlGraphEntity::BuiltinType(defined) => &*defined == retval.type_name,
                            _ => false,
                        })
                        .ok_or_else(|| eyre!("Could not find return type in graph."))?;
                    use crate::sql_entity_graph::metadata::{ReturnVariant, SqlVariant};

                    let (variant_prefix, sql_type) = match retval.return_sql {
                        Ok(ReturnVariant::Plain(ref variant)) => ("", match variant {
                            SqlVariant::Mapped(ref sql) => sql.clone(),
                            SqlVariant::Composite { requires_array_brackets } => match &self.fn_return {
                                PgExternReturnEntity::None => return Err(eyre!("Macro expansion time suggested no return value, but at runtime a return value was determined")),
                                PgExternReturnEntity::Type { ty } => ty.composite_type.map(|v| if *requires_array_brackets { format!("{v}[]") } else { format!("{v}") }).ok_or_else(|| eyre!("Macro expansion time suggested a composite_type!() in return"))?,
                                PgExternReturnEntity::SetOf { .. } => return Err(eyre!("Macro expansion time suggested a SetOfIterator return value, but at runtime a plain return value was determined")),
                                PgExternReturnEntity::Iterated(_) => return Err(eyre!("Macro expansion time suggested a TableIterator return value, but at runtime a plain return value was determined")),
                                PgExternReturnEntity::Trigger => return Err(eyre!("Macro expansion time suggested a Trigger return value, but at runtime a plain return value was determined")),
                            },
                            SqlVariant::Skip => return Err(eyre!("At runtime a skipped return value was determined, this is not valid")),
                        }),
                        Ok(ReturnVariant::SetOf(ref variant)) => ("SETOF ", match variant {
                            SqlVariant::Mapped(ref sql) => sql.clone(),
                            SqlVariant::Composite { requires_array_brackets } => match &self.fn_return {
                                PgExternReturnEntity::None => return Err(eyre!("Macro expansion time suggested no return value, but at runtime a return value was determined")),
                                PgExternReturnEntity::Type { .. } => return Err(eyre!("Macro expansion time suggested a plain return value, but at runtime a SetOf return value was determined")),
                                PgExternReturnEntity::SetOf { ty } => ty.composite_type.map(|v| if *requires_array_brackets { format!("{v}[]") } else { format!("{v}") }).ok_or_else(|| eyre!("Macro expansion time suggested a composite_type!() in return"))?,
                                PgExternReturnEntity::Iterated(_) => return Err(eyre!("Macro expansion time suggested a TableIterator return value, but at runtime a SetOf return value was determined")),
                                PgExternReturnEntity::Trigger => return Err(eyre!("Macro expansion time suggested a Trigger return value, but at runtime a SetOf return value was determined")),
                            },
                            SqlVariant::Skip => todo!(),
                        }),
                        Ok(ReturnVariant::Table(ref vec_of_variant)) => ("TABLE ", "TODO".into()),
                        Err(err) => return Err(err).wrap_err("Mapping return type"),
                    };
                    format!(
                        "RETURNS {variant_prefix}{schema_prefix}{sql_type} /* {type_name} */",
                        variant_prefix = variant_prefix,
                        sql_type = sql_type,
                        schema_prefix = context.schema_prefix_for(&graph_index),
                        type_name = retval.type_name
                    )
                }
            },
            search_path = if let Some(search_path) = &self.search_path {
                let retval = format!("SET search_path TO {}", search_path.join(", "));
                retval + "\n"
            } else {
                Default::default()
            },
            extern_attrs = if extern_attrs.is_empty() {
                String::default()
            } else {
                let mut retval = extern_attrs
                    .iter()
                    .map(|attr| format!("{}", attr).to_uppercase())
                    .collect::<Vec<_>>()
                    .join(" ");
                retval.push('\n');
                retval
            },
        );

        let ext_sql = format!(
            "\n\
                                -- {file}:{line}\n\
                                -- {module_path}::{name}\n\
                                {requires}\
                                {fn_sql}\
                            ",
            name = self.name,
            module_path = self.module_path,
            file = self.file,
            line = self.line,
            fn_sql = fn_sql,
            requires = {
                let requires_attrs = self
                    .extern_attrs
                    .iter()
                    .filter_map(|x| match x {
                        ExternArgs::Requires(requirements) => Some(requirements),
                        _ => None,
                    })
                    .flatten()
                    .collect::<Vec<_>>();
                if !requires_attrs.is_empty() {
                    format!(
                        "\
                       -- requires:\n\
                        {}\n\
                    ",
                        requires_attrs
                            .iter()
                            .map(|i| format!("--   {}", i))
                            .collect::<Vec<_>>()
                            .join("\n")
                    )
                } else {
                    "".to_string()
                }
            },
        );
        tracing::trace!(sql = %ext_sql);

        let rendered = if let Some(op) = &self.operator {
            let mut optionals = vec![];
            if let Some(it) = op.commutator {
                optionals.push(format!("\tCOMMUTATOR = {}", it));
            };
            if let Some(it) = op.negator {
                optionals.push(format!("\tNEGATOR = {}", it));
            };
            if let Some(it) = op.restrict {
                optionals.push(format!("\tRESTRICT = {}", it));
            };
            if let Some(it) = op.join {
                optionals.push(format!("\tJOIN = {}", it));
            };
            if op.hashes {
                optionals.push(String::from("\tHASHES"));
            };
            if op.merges {
                optionals.push(String::from("\tMERGES"));
            };

            let left_arg =
                self.metadata.arguments.get(0).ok_or_else(|| {
                    eyre!("Did not find `left_arg` for operator `{}`.", self.name)
                })?;
            let left_arg_graph_index = context
                .graph
                .neighbors_undirected(self_index)
                .find(|neighbor| match &context.graph[*neighbor] {
                    SqlGraphEntity::Type(ty) => ty.id_matches(&left_arg.type_id),
                    SqlGraphEntity::Enum(en) => en.id_matches(&left_arg.type_id),
                    SqlGraphEntity::BuiltinType(defined) => defined == &left_arg.type_name,
                    _ => false,
                })
                .ok_or_else(|| {
                    eyre!("Could not find left arg type in graph. Got: {:?}", left_arg)
                })?;
            let left_arg_sql = match left_arg.argument_sql {
                Ok(SqlVariant::Mapped(ref sql)) => sql.clone(),
                Ok(SqlVariant::Composite {
                    requires_array_brackets,
                }) => {
                    if requires_array_brackets {
                        let composite_type = self.fn_args[0].used_ty.composite_type
                            .ok_or(eyre!("Found a composite type but macro expansion time did not reveal a name, use `pgx::composite_type!()`"))?;
                        format!("{composite_type}[]")
                    } else {
                        self.fn_args[0].used_ty.composite_type
                            .ok_or(eyre!("Found a composite type but macro expansion time did not reveal a name, use `pgx::composite_type!()`"))?.to_string()
                    }
                }
                Ok(SqlVariant::Skip) => {
                    return Err(eyre!(
                        "Found an skipped SQL type in an operator, this is not valid"
                    ))
                }
                Err(err) => return Err(err.into()),
            };

            let right_arg =
                self.metadata.arguments.get(1).ok_or_else(|| {
                    eyre!("Did not find `left_arg` for operator `{}`.", self.name)
                })?;
            let right_arg_graph_index = context
                .graph
                .neighbors_undirected(self_index)
                .find(|neighbor| match &context.graph[*neighbor] {
                    SqlGraphEntity::Type(ty) => ty.id_matches(&right_arg.type_id),
                    SqlGraphEntity::Enum(en) => en.id_matches(&right_arg.type_id),
                    SqlGraphEntity::BuiltinType(defined) => defined == &right_arg.type_name,
                    _ => false,
                })
                .ok_or_else(|| {
                    eyre!(
                        "Could not find right arg type in graph. Got: {:?}",
                        right_arg
                    )
                })?;
            let right_arg_sql = match right_arg.argument_sql {
                Ok(SqlVariant::Mapped(ref sql)) => sql.clone(),
                Ok(SqlVariant::Composite {
                    requires_array_brackets,
                }) => {
                    if requires_array_brackets {
                        let composite_type = self.fn_args[1].used_ty.composite_type
                            .ok_or(eyre!("Found a composite type but macro expansion time did not reveal a name, use `pgx::composite_type!()`"))?;
                        format!("{composite_type}[]")
                    } else {
                        self.fn_args[0].used_ty.composite_type
                            .ok_or(eyre!("Found a composite type but macro expansion time did not reveal a name, use `pgx::composite_type!()`"))?.to_string()
                    }
                }
                Ok(SqlVariant::Skip) => {
                    return Err(eyre!(
                        "Found an skipped SQL type in an operator, this is not valid"
                    ))
                }
                Err(err) => return Err(err.into()),
            };

            let operator_sql = format!("\n\n\
                                                    -- {file}:{line}\n\
                                                    -- {module_path}::{name}\n\
                                                    CREATE OPERATOR {opname} (\n\
                                                        \tPROCEDURE=\"{name}\",\n\
                                                        \tLEFTARG={schema_prefix_left}{left_arg}, /* {left_name} */\n\
                                                        \tRIGHTARG={schema_prefix_right}{right_arg}{maybe_comma} /* {right_name} */\n\
                                                        {optionals}\
                                                    );\
                                                    ",
                                                    opname = op.opname.unwrap(),
                                                    file = self.file,
                                                    line = self.line,
                                                    name = self.name,
                                                    module_path = self.module_path,
                                                    left_name = left_arg.type_name,
                                                    right_name = right_arg.type_name,
                                                    schema_prefix_left = context.schema_prefix_for(&left_arg_graph_index),
                                                    left_arg = left_arg_sql,
                                                    schema_prefix_right = context.schema_prefix_for(&right_arg_graph_index),
                                                    right_arg = right_arg_sql,
                                                    maybe_comma = if optionals.len() >= 1 { "," } else { "" },
                                                    optionals = if !optionals.is_empty() { optionals.join(",\n") + "\n" } else { "".to_string() },
                                            );
            tracing::trace!(sql = %operator_sql);
            ext_sql + &operator_sql
        } else {
            ext_sql
        };
        Ok(rendered)
    }
}
