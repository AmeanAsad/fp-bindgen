use crate::functions::FunctionList;
use crate::prelude::Primitive;
use crate::types::{format_name_with_generics, EnumOptions, Field, GenericArgument, Type, Variant};
use inflector::Inflector;
use std::collections::BTreeSet;
use std::fs;

pub fn generate_bindings(
    import_functions: FunctionList,
    export_functions: FunctionList,
    serializable_types: BTreeSet<Type>,
    mut deserializable_types: BTreeSet<Type>,
    path: &str,
) {
    let mut all_types = serializable_types;
    all_types.append(&mut deserializable_types);

    generate_type_bindings(&all_types, path);

    let import_decls = format_function_declarations(&import_functions, FunctionType::Import);
    let export_decls = format_function_declarations(&export_functions, FunctionType::Export);

    let type_names = all_types
        .into_iter()
        .filter_map(|ty| match ty {
            Type::Enum(name, _, _, _) => Some(name),
            Type::Struct(name, _, _) => Some(name),
            _ => None,
        })
        .collect::<Vec<_>>();

    let import_wrappers = format_import_wrappers(&import_functions);
    let export_wrappers = format_export_wrappers(&export_functions);

    let contents = format!(
        "import {{ encode, decode }} from \"@msgpack/msgpack\";

import type {{
{}
}} from \"./types\";

type FatPtr = bigint;

export type Imports = {{
{}
}};

export type Exports = {{
{}
}};

/**
 * Represents an unrecoverable error in the FP runtime.
 *
 * After this, your only recourse is to create a new runtime, probably with a different WASM plugin.
 */
export class FPRuntimeError extends Error {{
    constructor(message: string) {{
        super(message);
    }}
}}

/**
 * Creates a runtime for executing the given plugin.
 *
 * @param plugin The raw WASM plugin.
 * @param importFunctions The host functions that may be imported by the plugin.
 * @returns The functions that may be exported by the plugin.
 */
export async function createRuntime(
    plugin: ArrayBuffer,
    importFunctions: Imports
): Promise<Exports> {{
    const promises = new Map<FatPtr, (result: unknown) => void>();

    function assignAsyncValue<T>(fatPtr: FatPtr, result: T) {{
        const [ptr, len] = fromFatPtr(fatPtr);
        const buffer = new Uint32Array(memory.buffer, ptr, len / 4);
        const [resultPtr, resultLen] = fromFatPtr(serializeObject(result));
        buffer[1] = resultPtr;
        buffer[2] = resultLen;
        buffer[0] = 1; // Set status to ready.
    }}

    function createAsyncValue(): FatPtr {{
        const len = 12; // std::mem::size_of::<AsyncValue>()
        const fatPtr = malloc(len);
        const [ptr] = fromFatPtr(fatPtr);
        const buffer = new Uint8Array(memory.buffer, ptr, len);
        buffer.fill(0);
        return fatPtr;
    }}

    function parseObject<T>(fatPtr: FatPtr): T {{
        const [ptr, len] = fromFatPtr(fatPtr);
        const buffer = new Uint8Array(memory.buffer, ptr, len);
        const object = decode<T>(buffer) as T;
        free(fatPtr);
        return object;
    }}

    function promiseFromPtr<T>(ptr: FatPtr): Promise<T> {{
        return new Promise<T>((resolve) => {{
            promises.set(ptr, resolve as (result: unknown) => void);
        }});
    }}

    function resolvePromise(ptr: FatPtr) {{
        const resolve = promises.get(ptr);
        if (resolve) {{
            const [asyncPtr, asyncLen] = fromFatPtr(ptr);
            const buffer = new Uint32Array(memory.buffer, asyncPtr, asyncLen / 4);
            switch (buffer[0]) {{
                case 0:
                    throw new FPRuntimeError(\"Tried to resolve promise that is not ready\");
                case 1:
                    resolve(parseObject(toFatPtr(buffer[1]!, buffer[2]!)));
                    break;
                default:
                    throw new FPRuntimeError(\"Unexpected status: \" + buffer[0]);
            }}
        }} else {{
            throw new FPRuntimeError(\"Tried to resolve unknown promise\");
        }}
    }}

    function serializeObject<T>(object: T): FatPtr {{
        const serialized = encode(object);
        const fatPtr = malloc(serialized.length);
        const [ptr, len] = fromFatPtr(fatPtr);
        const buffer = new Uint8Array(memory.buffer, ptr, len);
        buffer.set(serialized);
        return fatPtr;
    }}

    const {{ instance }} = await WebAssembly.instantiate(plugin, {{
        fp: {{
            __fp_host_resolve_async_value: resolvePromise,
{}
        }},
    }});

    const getExport = <T>(name: string): T => {{
        const exp = instance.exports[name];
        if (!exp) {{
            throw new FPRuntimeError(`Plugin did not export expected symbol: \"${{name}}\"`);
        }}
        return exp as unknown as T;
    }};

    const memory = getExport<WebAssembly.Memory>(\"memory\");
    const malloc = getExport<(len: number) => FatPtr>(\"__fp_malloc\");
    const free = getExport<(ptr: FatPtr) => void>(\"__fp_free\");
    const resolveFuture = getExport<(ptr: FatPtr) => void>(\"__fp_guest_resolve_async_value\");

    return {{
{}
    }};
}}

function fromFatPtr(fatPtr: FatPtr): [ptr: number, len: number] {{
    return [
        Number.parseInt((fatPtr >> 32n).toString()),
        Number.parseInt((fatPtr & 0xffff_ffffn).toString()),
    ];
}}

function toFatPtr(ptr: number, len: number): FatPtr {{
    return (BigInt(ptr) << 32n) | BigInt(len);
}}
",
        join_lines(&type_names, |line| format!("    {},", line)),
        join_lines(&import_decls, |line| format!("    {};", line)),
        join_lines(&export_decls, |line| format!("    {};", line)),
        join_lines(&import_wrappers, |line| format!("            {}", line)),
        join_lines(&export_wrappers, |line| format!("        {}", line)),
    );
    write_bindings_file(format!("{}/index.ts", path), &contents);
}

enum FunctionType {
    Import,
    Export,
}

fn format_function_declarations(
    functions: &FunctionList,
    function_type: FunctionType,
) -> Vec<String> {
    // Plugins can always omit exports, while runtimes are always expected to provide all imports:
    let optional_marker = match function_type {
        FunctionType::Import => "",
        FunctionType::Export => "?",
    };

    functions
        .iter()
        .map(|function| {
            let args = function
                .args
                .iter()
                .map(|arg| format!("{}: {}", arg.name.to_camel_case(), format_type(&arg.ty)))
                .collect::<Vec<_>>()
                .join(", ");
            let return_type = if function.is_async {
                format!(" => Promise<{}>", format_type(&function.return_type))
            } else {
                format!(" => {}", format_type(&function.return_type))
            };
            format!(
                "{}{}: ({}){}",
                function.name.to_camel_case(),
                optional_marker,
                args,
                return_type
            )
        })
        .collect()
}

fn format_import_wrappers(import_functions: &FunctionList) -> Vec<String> {
    import_functions
        .into_iter()
        .flat_map(|function| {
            let name = &function.name;
            let args_with_ptr_types = function
                .args
                .iter()
                .map(|arg| match &arg.ty {
                    Type::Primitive(primitive) => format!(
                        "{}: {}",
                        arg.name.to_camel_case(),
                        format_primitive(*primitive)
                    ),
                    _ => format!("{}_ptr: FatPtr", arg.name),
                })
                .collect::<Vec<_>>()
                .join(", ");
            let return_type = match &function.return_type {
                Type::Unit => "".to_owned(),
                Type::Primitive(primitive) => format!(": {}", format_primitive(*primitive)),
                _ => ": FatPtr".to_owned(),
            };
            let import_args = function
                .args
                .iter()
                .filter_map(|arg| match &arg.ty {
                    Type::Primitive(_) => None,
                    ty => Some(format!(
                        "const {} = parseObject<{}>({}_ptr);",
                        arg.name.to_camel_case(),
                        format_type(ty),
                        arg.name
                    )),
                })
                .collect::<Vec<_>>();
            let args = function
                .args
                .iter()
                .map(|arg| arg.name.to_camel_case())
                .collect::<Vec<_>>()
                .join(", ");
            if function.is_async {
                let assign_async_value = match &function.return_type {
                    Type::Unit => "",
                    _ => "\n            assignAsyncValue(_async_result_ptr, result);",
                };

                format!(
                    "__fp_gen_{}: ({}){} => {{
{}    const _async_result_ptr = createAsyncValue();
    importFunctions.{}({})
        .then((result) => {{{}
            resolveFuture(_async_result_ptr);
        }})
        .catch((error) => {{
            console.error(
                'Unrecoverable exception trying to call async host function \"{}\"',
                error
            );
        }});
    return _async_result_ptr;
}},",
                    name,
                    args_with_ptr_types,
                    return_type,
                    import_args
                        .iter()
                        .map(|line| format!("    {}\n", line))
                        .collect::<Vec<_>>()
                        .join(""),
                    name.to_camel_case(),
                    args,
                    assign_async_value,
                    name
                )
                .split('\n')
                .map(|line| line.to_owned())
                .collect::<Vec<_>>()
            } else {
                let fn_call = match &function.return_type {
                    Type::Unit => format!("importFunctions.{}({});", name.to_camel_case(), args),
                    Type::Primitive(_) => {
                        format!("return importFunctions.{}({});", name.to_camel_case(), args)
                    }
                    _ => format!(
                        "return serializeObject(importFunctions.{}({}));",
                        name.to_camel_case(),
                        args
                    ),
                };

                format!(
                    "__fp_gen_{}: ({}){} => {{\n{}    {}\n}},",
                    name,
                    args_with_ptr_types,
                    return_type,
                    import_args
                        .iter()
                        .map(|line| format!("    {}\n", line))
                        .collect::<Vec<_>>()
                        .join(""),
                    fn_call
                )
                .split('\n')
                .map(|line| line.to_owned())
                .collect::<Vec<_>>()
            }
        })
        .collect()
}

fn format_export_wrappers(import_functions: &FunctionList) -> Vec<String> {
    import_functions
        .into_iter()
        .flat_map(|function| {
            let name = &function.name;
            let args = function
                .args
                .iter()
                .map(|arg| format!("{}: {}", arg.name.to_camel_case(), format_type(&arg.ty)))
                .collect::<Vec<_>>()
                .join(", ");
            let export_args = function
                .args
                .iter()
                .filter_map(|arg| match &arg.ty {
                    Type::Primitive(_) => None,
                    _ => Some(format!(
                        "const {}_ptr = serializeObject({});",
                        arg.name,
                        arg.name.to_camel_case()
                    )),
                })
                .collect::<Vec<_>>();

            // Trivial functions can simply be returned as is:
            if export_args.is_empty() && !function.is_async {
                return vec![format!(
                    "{}: instance.exports.__fp_gen_{} as any,",
                    name.to_camel_case(),
                    name
                )];
            }

            let call_args = function
                .args
                .iter()
                .map(|arg| match &arg.ty {
                    Type::Primitive(_) => arg.name.to_camel_case(),
                    _ => format!("{}_ptr", arg.name),
                })
                .collect::<Vec<_>>()
                .join(", ");
            let fn_call = if function.is_async {
                format!(
                    "return promiseFromPtr<{}>(export_fn({}));",
                    format_type(&function.return_type),
                    call_args
                )
            } else {
                match &function.return_type {
                    Type::Unit => format!("export_fn({});", call_args),
                    Type::Primitive(_) => {
                        format!("return export_fn({});", call_args)
                    }
                    ty => format!(
                        "return parseObject<{}>(export_fn({}));",
                        format_type(ty),
                        call_args
                    ),
                }
            };
            let return_fn = if export_args.is_empty() {
                format!("return ({}) => {}", args, fn_call.replace("return ", ""))
            } else {
                format!(
                    "return ({}) => {{\n{}\n        {}\n    }};",
                    args,
                    join_lines(&export_args, |line| format!("        {}", line)),
                    fn_call
                )
            };
            format!(
                "{}: (() => {{
    const export_fn = instance.exports.__fp_gen_{} as any;
    if (!export_fn) return;

    {}
}})(),",
                name.to_camel_case(),
                name,
                return_fn
            )
            .split('\n')
            .map(|line| line.to_owned())
            .collect::<Vec<_>>()
        })
        .collect()
}

fn generate_type_bindings(types: &BTreeSet<Type>, path: &str) {
    let type_defs = types
        .iter()
        .filter_map(|ty| match ty {
            Type::Alias(name, ty) => Some(format!(
                "export type {} = {};",
                name,
                format_type(ty.as_ref())
            )),
            Type::Enum(name, generic_args, variants, opts) => Some(create_enum_definition(
                name,
                generic_args,
                variants,
                opts.clone(),
            )),
            Type::Struct(name, generic_args, fields) => {
                Some(create_struct_definition(name, generic_args, fields))
            }
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n\n");

    write_bindings_file(format!("{}/types.ts", path), format!("{}\n", type_defs))
}

fn create_enum_definition(
    name: &str,
    generic_args: &[GenericArgument],
    variants: &[Variant],
    opts: EnumOptions,
) -> String {
    let variants = variants
        .iter()
        .map(|variant| {
            let variant_name = opts.variant_casing.format_string(&variant.name);
            match &variant.ty {
                Type::Unit => {
                    if let Some(tag) = &opts.tag_prop_name {
                        format!("    | {{ {}: \"{}\" }}", tag, variant_name)
                    } else {
                        format!("    | \"{}\"", variant_name)
                    }
                }
                Type::Struct(_, _, fields) => {
                    if opts.untagged {
                        format!("    | {{ {} }}", format_struct_fields(fields).join("; "))
                    } else {
                        match (&opts.tag_prop_name, &opts.content_prop_name) {
                            (Some(tag), Some(content)) => {
                                format!(
                                    "    | {{ {}: \"{}\"; {}: {{ {} }} }}",
                                    tag,
                                    variant_name,
                                    content,
                                    format_struct_fields(fields).join("; ")
                                )
                            }
                            (Some(tag), None) => {
                                format!(
                                    "    | {{ {}: \"{}\"; {} }}",
                                    tag,
                                    variant_name,
                                    format_struct_fields(fields).join("; ")
                                )
                            }
                            (None, _) => {
                                format!(
                                    "    | {{ {}: {{ {} }} }}",
                                    variant_name,
                                    format_struct_fields(fields).join("; ")
                                )
                            }
                        }
                    }
                }
                Type::Tuple(items) if items.len() == 1 => {
                    if opts.untagged {
                        format!("    | {}", format_type(items.first().unwrap()))
                    } else {
                        match (&opts.tag_prop_name, &opts.content_prop_name) {
                            (Some(tag), Some(content)) => {
                                format!(
                                    "    | {{ {}: \"{}\"; {}: {} }}",
                                    tag,
                                    variant_name,
                                    content,
                                    format_type(items.first().unwrap())
                                )
                            }
                            (Some(tag), None) => {
                                format!(
                                    "    | {{ {}: \"{}\" }} & {}",
                                    tag,
                                    variant_name,
                                    format_type(items.first().unwrap())
                                )
                            }
                            (None, _) => {
                                format!(
                                    "    | {{ {}: {} }}",
                                    variant_name,
                                    format_type(items.first().unwrap())
                                )
                            }
                        }
                    }
                }
                other => panic!("Unsupported type for enum variant: {:?}", other),
            }
        })
        .collect::<Vec<_>>()
        .join("\n");

    format!(
        "export type {} =\n{};",
        format_name_with_generics(name, generic_args),
        variants
    )
}

fn create_struct_definition(
    name: &str,
    generic_args: &[GenericArgument],
    fields: &[Field],
) -> String {
    format!(
        "export type {} = {{\n{}\n}};",
        format_name_with_generics(name, generic_args),
        join_lines(&format_struct_fields(fields), |line| format!(
            "    {};",
            line
        ))
    )
}

fn format_name_with_types(name: &str, generic_args: &[GenericArgument]) -> String {
    if generic_args.is_empty() {
        name.to_owned()
    } else {
        format!(
            "{}<{}>",
            name,
            generic_args
                .iter()
                .map(|arg| match &arg.ty {
                    Some(ty) => format_type(ty),
                    None => arg.name.clone(),
                })
                .collect::<Vec<_>>()
                .join(", ")
        )
    }
}

fn format_struct_fields(fields: &[Field]) -> Vec<String> {
    fields
        .iter()
        .map(|field| match &field.ty {
            Type::Container(name, ty) => {
                let optional = if name == "Option" { "?" } else { "" };
                format!(
                    "{}{}: {}",
                    field.name.to_camel_case(),
                    optional,
                    format_type(ty)
                )
            }
            ty => format!("{}: {}", field.name.to_camel_case(), format_type(ty)),
        })
        .collect()
}

/// Formats a type so it's valid TypeScript.
fn format_type(ty: &Type) -> String {
    match ty {
        Type::Alias(name, _) => name.clone(),
        Type::Container(name, ty) => {
            if name == "Option" {
                format!("{} | null", format_type(ty))
            } else {
                format_type(ty)
            }
        }
        Type::Custom(custom) => custom.ts_ty.clone(),
        Type::Enum(name, generic_args, _, _) => format_name_with_types(name, generic_args),
        Type::GenericArgument(arg) => arg.name.clone(),
        Type::List(_, ty) => {
            if ty.as_ref() == &Type::Primitive(Primitive::U8) {
                // Special case so `Vec<u8>` becomes `ArrayBuffer` in TS:
                "ArrayBuffer".to_owned()
            } else {
                format!("Array<{}>", format_type(ty))
            }
        }
        Type::Map(_, k, v) => format!("Record<{}, {}>", format_type(k), format_type(v)),
        Type::Primitive(primitive) => format_primitive(*primitive),
        Type::String => "string".to_owned(),
        Type::Struct(name, generic_args, _) => format_name_with_types(name, generic_args),
        Type::Tuple(items) => format!(
            "[{}]",
            items
                .iter()
                .map(|item| item.name())
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Type::Unit => "void".to_owned(),
    }
}

fn format_primitive(primitive: Primitive) -> String {
    let string = match primitive {
        Primitive::Bool => "boolean",
        Primitive::F32 => "number",
        Primitive::F64 => "number",
        Primitive::I8 => "number",
        Primitive::I16 => "number",
        Primitive::I32 => "number",
        Primitive::I64 => "bigint",
        Primitive::I128 => "bigint",
        Primitive::U8 => "number",
        Primitive::U16 => "number",
        Primitive::U32 => "number",
        Primitive::U64 => "bigint",
        Primitive::U128 => "bigint",
    };
    string.to_owned()
}

fn join_lines<F>(lines: &[String], formatter: F) -> String
where
    F: FnMut(&String) -> String,
{
    lines.iter().map(formatter).collect::<Vec<_>>().join("\n")
}

fn write_bindings_file<C>(file_path: String, contents: C)
where
    C: AsRef<[u8]>,
{
    fs::write(&file_path, &contents).expect("Could not write bindings file");
}