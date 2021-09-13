mod casing;
mod functions;
mod generators;
mod primitives;
mod serializable;
mod types;

pub mod prelude;

use fp_bindgen_macros::primitive_impls;
use prelude::*;
use std::{collections::BTreeSet, fs, str::FromStr};

primitive_impls!();

enum BindingsType {
    RustPlugin,
    TsRuntime,
}

impl FromStr for BindingsType {
    type Err = String;

    fn from_str(bindings_type: &str) -> Result<Self, Self::Err> {
        match bindings_type {
            "rust-plugin" => Ok(Self::RustPlugin),
            "ts-runtime" => Ok(Self::TsRuntime),
            other => Err(format!(
                "Bindings type must be one of `rust-plugin`, `ts-runtime`, was: `{}`",
                other
            )),
        }
    }
}

pub fn generate_bindings(
    import_functions: FunctionList,
    export_functions: FunctionList,
    serializable_types: BTreeSet<Type>,
    deserializable_types: BTreeSet<Type>,
    bindings_type: &str,
    path: &str,
) {
    let bindings_type = BindingsType::from_str(bindings_type).expect("Unknown bindings type");

    fs::create_dir_all(path).expect("Could not create output directory");

    match bindings_type {
        BindingsType::RustPlugin => generators::rust_plugin::generate_bindings(
            import_functions,
            export_functions,
            serializable_types,
            deserializable_types,
            path,
        ),
        BindingsType::TsRuntime => generators::ts_runtime::generate_bindings(
            import_functions,
            export_functions,
            serializable_types,
            deserializable_types,
            path,
        ),
    }
}
