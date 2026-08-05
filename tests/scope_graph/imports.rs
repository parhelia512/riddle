use crate::{
    DefKind, build, build_hir_and_graph, child_module_id, module_id_by_name, resolve_paths,
    resolved_struct_id, struct_id_in_module, top_level_struct_id,
};

#[test]
fn use_alias_rewrites_to_target_path() {
    let sg = build(
        r"
        mod m {
            pub struct S {}
        }

        use crate::m::S as T;

        fun f() {
            T
        }
        ",
    );

    assert_eq!(resolve_paths(&sg, "T"), vec![vec![DefKind::Struct]]);
}

#[test]
fn glob_import_resolves_modules_step_by_step() {
    let sg = build(
        r"
        mod b {
            pub mod target {
                pub struct B {}
            }
        }

        mod a {
            pub mod target {
                pub struct A {}
            }
        }

        use crate::a::target::*;

        fun f() {
            A;
            B
        }
        ",
    );

    assert_eq!(resolve_paths(&sg, "A"), vec![vec![DefKind::Struct]]);
    assert_eq!(resolve_paths(&sg, "B"), vec![vec![]]);
}

#[test]
fn unresolved_glob_prefix_does_not_import_parent_module() {
    let sg = build(
        r"
        mod a {
            struct S {}
        }

        use crate::a::missing::*;

        fun f() {
            S
        }
        ",
    );

    assert_eq!(resolve_paths(&sg, "S"), vec![vec![]]);
}

#[test]
fn self_super_and_crate_aliases_bind_to_the_right_structs() {
    let (hir, sg) = build_hir_and_graph(
        r"
        struct S {}

        pub mod outer {
            struct S {}

            pub mod inner {
                struct S {}

                use self::S as LocalS;
                use super::S as OuterS;
                use crate::S as CrateS;

                fun f() {
                    LocalS;
                    OuterS;
                    CrateS;
                }
            }
        }
        ",
    );

    let root_struct = top_level_struct_id(&hir, "S");
    let parent_module = module_id_by_name(&hir, "outer");
    let nested_module = child_module_id(&hir, parent_module, "inner");
    let parent_struct = struct_id_in_module(&hir, parent_module, "S");
    let nested_struct = struct_id_in_module(&hir, nested_module, "S");

    assert_eq!(resolved_struct_id(&sg, "LocalS"), nested_struct);
    assert_eq!(resolved_struct_id(&sg, "OuterS"), parent_struct);
    assert_eq!(resolved_struct_id(&sg, "CrateS"), root_struct);
}
