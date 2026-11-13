//! The keywords of Snakemake 9.27.0: global statements, rule directives and module settings.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

/// Defines a keyword enum with the exact spelling that Snakefiles use for each variant.
macro_rules! keywords {
    ($(#[$meta:meta])* $name:ident { $($variant:ident = $text:literal,)* }) => {
        $(#[$meta])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        pub enum $name {
            $(
                #[doc = concat!("`", $text, "`")]
                $variant,
            )*
        }

        impl $name {
            pub const ALL: &[Self] = &[$(Self::$variant,)*];

            /// The keyword as written in a Snakefile.
            pub fn text(self) -> &'static str {
                match self {
                    $(Self::$variant => $text,)*
                }
            }

            pub fn from_text(text: &str) -> Option<Self> {
                match text {
                    $($text => Some(Self::$variant),)*
                    _ => None,
                }
            }
        }
    };
}

keywords! {
    /// A global keyword whose statement is a keyword and a value, like `include: "rules/a.smk"`.
    GlobalKeyword {
        EnvironmentVariables = "envvars",
        Include = "include",
        WorkingDirectory = "workdir",
        ConfigFile = "configfile",
        PathVariables = "pathvars",
        PepFile = "pepfile",
        PepSchema = "pepschema",
        Report = "report",
        RuleOrder = "ruleorder",
        LocalRules = "localrules",
        OnSuccess = "onsuccess",
        OnError = "onerror",
        OnStart = "onstart",
        WildcardConstraints = "wildcard_constraints",
        Singularity = "singularity",
        Container = "container",
        Containerized = "containerized",
        Conda = "conda",
        ScatterGather = "scattergather",
        InputFlags = "inputflags",
        OutputFlags = "outputflags",
        ResourceScopes = "resource_scopes",
    }
}

keywords! {
    /// A directive inside a rule, a checkpoint or a `use rule ... with:` block.
    RuleKeyword {
        Name = "name",
        Input = "input",
        Output = "output",
        Parameters = "params",
        Threads = "threads",
        Resources = "resources",
        Retries = "retries",
        Priority = "priority",
        Log = "log",
        Message = "message",
        Benchmark = "benchmark",
        Conda = "conda",
        Singularity = "singularity",
        Container = "container",
        Containerized = "containerized",
        EnvironmentModules = "envmodules",
        WildcardConstraints = "wildcard_constraints",
        Shadow = "shadow",
        Group = "group",
        Cache = "cache",
        Handover = "handover",
        DefaultTarget = "default_target",
        LocalRule = "localrule",
        PathVariables = "pathvars",
        Run = "run",
        Shell = "shell",
        Script = "script",
        Notebook = "notebook",
        Wrapper = "wrapper",
        TemplateEngine = "template_engine",
        Cwl = "cwl",
    }
}

keywords! {
    /// A setting inside a `module` block.
    ModuleKeyword {
        Name = "name",
        Snakefile = "snakefile",
        MetaWrapper = "meta_wrapper",
        Config = "config",
        SkipValidation = "skip_validation",
        ReplacePrefix = "replace_prefix",
        Prefix = "prefix",
        PathVariables = "pathvars",
    }
}

impl RuleKeyword {
    /// Whether this directive says how the rule runs; a rule has at most one, and it comes last.
    pub fn is_execution(self) -> bool {
        matches!(
            self,
            Self::Run
                | Self::Shell
                | Self::Script
                | Self::Notebook
                | Self::Wrapper
                | Self::TemplateEngine
                | Self::Cwl
        )
    }
}
