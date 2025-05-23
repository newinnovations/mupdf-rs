use std::env;
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Stdio;

// see https://github.com/ArtifexSoftware/mupdf/blob/master/source/fitz/noto.c
#[cfg(not(feature = "all-fonts"))]
const SKIP_FONTS: [&str; 6] = [
    "TOFU",
    "TOFU_CJK",
    "TOFU_NOTO",
    "TOFU_SYMBOL",
    "TOFU_EMOJI",
    "TOFU_SIL",
];

macro_rules! t {
    ($e:expr ) => {
        match $e {
            Ok(n) => n,
            Err(e) => panic!("\n{} failed with {}\n", stringify!($e), e),
        }
    };
}

fn cp_r(dir: &Path, dest: &Path, excluding_dir_names: &'static [&'static str]) {
    for entry in t!(fs::read_dir(dir)) {
        let entry = t!(entry);
        let path = entry.path();
        let dst = dest.join(path.file_name().expect("Failed to get filename of path"));
        if t!(fs::metadata(&path)).is_file() {
            fs::copy(&path, &dst)
                .unwrap_or_else(|e| panic!("Couldn't fs::copy {path:?} to {dst:?}: {e}"));
        } else if path
            .file_name()
            .and_then(OsStr::to_str)
            .is_none_or(|dst| !excluding_dir_names.contains(&dst))
        {
            t!(fs::create_dir_all(&dst));
            cp_r(&path, &dst, excluding_dir_names);
        }
    }
}

const CPU_FLAGS: &[(&str, &str, &str, Option<&str>)] = &[
    ("sse4.1", "-msse4.1", "HAVE_SSE4_1", Some("ARCH_HAS_SSE")),
    ("avx", "-mavx", "HAVE_AVX", None),
    ("avx2", "-mavx2", "HAVE_AVX2", None),
    ("fma", "-mfma", "HAVE_FMA", None),
    ("neon", "-mfpu=neon", "HAVE_NEON", Some("ARCH_HAS_NEON")),
];

#[cfg(not(target_env = "msvc"))]
fn build_libmupdf() {
    use std::process::Command;

    let features_var =
        std::env::var("CARGO_CFG_TARGET_FEATURE").expect("We need cargo to build this");
    let target_features = features_var.split(',').collect::<Vec<_>>();

    let profile = match &*env::var("PROFILE").unwrap_or("debug".to_owned()) {
        "bench" | "release" => "release",
        _ => "debug",
    };
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    let build_dir = out_dir.join("build");
    t!(fs::create_dir_all(&build_dir));

    // workaround for windows gnu toolchain, path separator is `/` but not `\`
    let build_dir_str = build_dir.to_string_lossy().replace("\\", "/");

    let current_dir = env::current_dir().unwrap();
    let mupdf_src_dir = current_dir.join("mupdf");
    cp_r(&mupdf_src_dir, &build_dir, &[".git"]);

    let mut build = cc::Build::new();
    #[cfg(not(feature = "xps"))]
    build.define("FZ_ENABLE_XPS", Some("0"));
    #[cfg(not(feature = "svg"))]
    build.define("FZ_ENABLE_SVG", Some("0"));
    #[cfg(not(feature = "cbz"))]
    build.define("FZ_ENABLE_CBZ", Some("0"));
    #[cfg(not(feature = "img"))]
    build.define("FZ_ENABLE_IMG", Some("0"));
    #[cfg(not(feature = "html"))]
    build.define("FZ_ENABLE_HTML", Some("0"));
    #[cfg(not(feature = "epub"))]
    build.define("FZ_ENABLE_EPUB", Some("0"));
    #[cfg(not(feature = "js"))]
    build.define("FZ_ENABLE_JS", Some("0"));

    #[cfg(not(feature = "all-fonts"))]
    SKIP_FONTS.iter().for_each(|font| {
        build.define(font, None);
    });

    let mut make_flags = vec![
        "libs".to_owned(),
        format!("build={}", profile),
        format!("OUT={}", &build_dir_str),
        #[cfg(not(feature = "tesseract"))]
        "USE_TESSERACT=no".to_owned(),
        #[cfg(not(feature = "libarchive"))]
        "USE_LIBARCHIVE=no".to_owned(),
        #[cfg(not(feature = "zxingcpp"))]
        "USE_ZXINGCPP=no".to_owned(),
        #[cfg(feature = "sys-lib")]
        "USE_SYSTEM_LIBS=yes".to_owned(),
        "HAVE_X11=no".to_owned(),
        "HAVE_GLUT=no".to_owned(),
        "HAVE_CURL=no".to_owned(),
        "verbose=yes".to_owned(),
    ];

    for (feature, flag, make_flag, define) in CPU_FLAGS {
        let contains = target_features.contains(feature);
        if contains {
            build.flag_if_supported(flag);

            make_flags.push(format!("{make_flag}=yes"));
        }

        if let Some(define) = define {
            build.define(define, if contains { "1" } else { "0" });
        }
    }

    // this may be unused if none of the features below are enabled
    #[allow(unused_variables, unused_mut)]
    let mut add_lib = |cflags_name: &'static str, pkgcfg_names: &[&str]| {
        make_flags.push(format!("USE_SYSTEM_{cflags_name}=yes"));
        for pkgcfg_name in pkgcfg_names {
            let cflags = pkg_config::probe_library(pkgcfg_name)
                .unwrap()
                .include_paths
                .iter()
                .map(|p| format!("-I{}", p.display()))
                .collect::<Vec<_>>()
                .join(" ");
            make_flags.push(format!("SYS_{cflags_name}_CFLAGS={cflags}"));
        }
    };

    #[cfg(any(feature = "sys-lib", feature = "sys-lib-freetype"))]
    add_lib("FREETYPE", &["freetype2"]);

    #[cfg(any(feature = "sys-lib", feature = "sys-lib-gumbo"))]
    add_lib("GUMBO", &["gumbo"]);

    #[cfg(any(feature = "sys-lib", feature = "sys-lib-harfbuzz"))]
    add_lib("HARFBUZZ", &["harfbuzz"]);

    #[cfg(any(feature = "sys-lib", feature = "sys-lib-jbig2dec"))]
    add_lib("JBIG2DEC", &["jbig2dec"]);

    // Exluded from sys-lib feature
    #[cfg(feature = "sys-lib-jpegxr")]
    add_lib("JPEGXR", &["jpegxr"]);

    // Exluded from sys-lib feature
    #[cfg(feature = "sys-lib-lcms2")]
    add_lib("LCMS2", &["lcms2"]);

    #[cfg(any(feature = "sys-lib", feature = "sys-lib-libjpeg"))]
    add_lib("LIBJPEG", &["libjpeg"]);

    #[cfg(any(feature = "sys-lib", feature = "sys-lib-openjpeg"))]
    add_lib("OPENJPEG", &["libopenjp2"]);

    #[cfg(any(feature = "sys-lib", feature = "sys-lib-zlib"))]
    add_lib("ZLIB", &["zlib"]);

    #[cfg(feature = "tesseract")]
    {
        #[cfg(any(feature = "sys-lib", feature = "sys-lib-leptonica"))]
        add_lib("LEPTONICA", &["lept"]);

        #[cfg(any(feature = "sys-lib", feature = "sys-lib-tesseract"))]
        add_lib("TESSERACT", &["tesseract"]);
    }

    #[cfg(all(
        feature = "zxingcpp",
        any(feature = "sys-lib", feature = "sys-lib-zxingcpp")
    ))]
    // zint is required as well, but it (or distro for that matter,
    // i checked debian, fedora and arch) distribute a pkg-config file
    add_lib("ZXINGCPP", &["zxing"]);

    #[cfg(feature = "libarchive")]
    add_lib("LIBARCHIVE", &["libarchive"]);

    #[cfg(any(feature = "sys-lib", feature = "sys-lib-brotli"))]
    add_lib("BROTLI", &["libbrotlidec", "libbrotlienc"]);

    // The mupdf Makefile does not do a very good job of detecting
    // and acting on cross-compilation, so we'll let the `cc` crate do it.
    let c_compiler = build.get_compiler();
    let cc = c_compiler.path().to_string_lossy();
    let c_flags = c_compiler.cflags_env();

    let cxx_compiler = build.cpp(true).get_compiler();
    let cxx = cxx_compiler.path().to_string_lossy();
    let cxx_flags = cxx_compiler.cflags_env();

    make_flags.push(format!("CC={}", cc));
    make_flags.push(format!("CXX={}", cxx));
    make_flags.push(format!("XCFLAGS={}", c_flags.to_string_lossy()));
    make_flags.push(format!("XCXXFLAGS={}", cxx_flags.to_string_lossy(),));

    // println!("cargo::warning=using make_flags {make_flags:?}");

    // Enable parallel compilation
    if let Ok(n) = std::thread::available_parallelism() {
        make_flags.push(format!("-j{}", n));
    }

    let make = if cfg!(any(
        target_os = "freebsd",
        target_os = "openbsd",
        target_os = "netbsd"
    )) {
        "gmake"
    } else {
        "make"
    };
    let output = Command::new(make)
        .args(&make_flags)
        .current_dir(&build_dir_str)
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .output()
        .expect("make failed");
    if !output.status.success() {
        panic!("Build error, exit code {}", output.status.code().unwrap());
    }
    println!("cargo:rustc-link-search=native={}", &build_dir_str);
    println!("cargo:rustc-link-lib=static=mupdf");
    // println!("cargo:rustc-link-lib=static=mupdf-pkcs7");
    println!("cargo:rustc-link-lib=static=mupdf-third");
    // println!("cargo:rustc-link-lib=static=mupdf-threads");
}

#[cfg(target_env = "msvc")]
fn build_libmupdf() {
    use cc::windows_registry::find_vs_version;

    // Patch geometry.c to compile on vs 2022
    let file_path = "mupdf/source/fitz/geometry.c";
    let content = fs::read_to_string(file_path).expect("Failed to read geometry.c file");
    let patched_content = content.replace("NAN", "(0.0/0.0)");
    fs::write(file_path, patched_content).expect("Failed to write patched geometry.c file");

    let target = env::var("TARGET").expect("TARGET not found in environment");
    let msvc_platform = if target.contains("x86_64") {
        "x64"
    } else {
        "Win32"
    };
    let profile = match &*env::var("PROFILE").unwrap_or("debug".to_owned()) {
        "bench" | "release" => "Release",
        _ => "Debug",
    };
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    let build_dir = out_dir.join("build");
    t!(fs::create_dir_all(&build_dir));

    let current_dir = env::current_dir().unwrap();
    let mupdf_src_dir = current_dir.join("mupdf");
    cp_r(&mupdf_src_dir, &build_dir, &[".git"]);

    let msbuild = cc::windows_registry::find(target.as_str(), "msbuild.exe");
    if let Some(mut msbuild) = msbuild {
        let mut cl_env = Vec::new();
        if !cfg!(feature = "all-fonts") {
            for font in &SKIP_FONTS {
                cl_env.push(format!("/D{}", font));
            }
        }
        if cfg!(not(feature = "xps")) {
            cl_env.push("/DFZ_ENABLE_XPS#0".to_string());
        }
        if cfg!(not(feature = "svg")) {
            cl_env.push("/DFZ_ENABLE_SVG#0".to_string());
        }
        if cfg!(not(feature = "cbz")) {
            cl_env.push("/DFZ_ENABLE_CBZ#0".to_string());
        }
        if cfg!(not(feature = "img")) {
            cl_env.push("/DFZ_ENABLE_IMG#0".to_string());
        }
        if cfg!(not(feature = "html")) {
            cl_env.push("/DFZ_ENABLE_HTML#0".to_string());
        }
        if cfg!(not(feature = "epub")) {
            cl_env.push("/DFZ_ENABLE_EPUB#0".to_string());
        }
        if cfg!(not(feature = "js")) {
            cl_env.push("/DFZ_ENABLE_JS#0".to_string());
        }

        // Enable parallel compilation
        cl_env.push("/MP".to_string());
        let platform_toolset = env::var("MUPDF_MSVC_PLATFORM_TOOLSET").unwrap_or(
            (match find_vs_version() {
                Ok(cc::windows_registry::VsVers::Vs17) => "v143",
                _ => "v142",
            })
            .to_string(),
        );
        let d = msbuild
            .args(&[
                "platform\\win32\\mupdf.sln",
                "/target:libmupdf",
                &format!("/p:Configuration={}", profile),
                &format!("/p:Platform={}", msvc_platform),
                &format!("/p:PlatformToolset={}", platform_toolset),
            ])
            .current_dir(&build_dir)
            .env("CL", cl_env.join(" "))
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .output()
            .expect("failed to run msbuild. Do you have it installed?");
        if !d.status.success() {
            let err = String::from_utf8_lossy(&d.stderr);
            let out = String::from_utf8_lossy(&d.stdout);
            panic!("Build error:\nSTDERR:{}\nSTDOUT:{}", err, out);
        }
        if msvc_platform == "x64" {
            println!(
                "cargo:rustc-link-search=native={}/platform/win32/{}/{}",
                build_dir.display(),
                msvc_platform,
                profile
            );
        } else {
            println!(
                "cargo:rustc-link-search=native={}/platform/win32/{}",
                build_dir.display(),
                profile
            );
        }

        if profile == "Debug" {
            println!("cargo:rustc-link-lib=dylib=ucrtd");
            println!("cargo:rustc-link-lib=dylib=vcruntimed");
            println!("cargo:rustc-link-lib=dylib=msvcrtd");
        }

        println!("cargo:rustc-link-lib=dylib=libmupdf");
        println!("cargo:rustc-link-lib=dylib=libthirdparty");
    } else {
        panic!("failed to find msbuild. Do you have it installed?");
    }
}

#[derive(Debug)]
struct Callback {
    types: regex::Regex,
    full_names: std::cell::RefCell<std::collections::HashMap<String, String>>,
}

impl Default for Callback {
    fn default() -> Self {
        Self {
            types: regex::RegexBuilder::new("fz_[a-z_*]+")
                .case_insensitive(true)
                .build()
                .unwrap(),
            full_names: std::cell::RefCell::default(),
        }
    }
}

impl bindgen::callbacks::ParseCallbacks for Callback {
    fn item_name(&self, original_item_name: &str) -> Option<String> {
        self.full_names
            .borrow_mut()
            .insert(original_item_name.to_owned(), original_item_name.to_owned());
        None
    }

    fn enum_variant_name(
        &self,
        enum_name: Option<&str>,
        original_variant_name: &str,
        _variant_value: bindgen::callbacks::EnumVariantValue,
    ) -> Option<String> {
        let enum_name = enum_name?;
        if enum_name.contains("unnamed at ") {
            return None;
        }

        let name = format!("{}_{}", enum_name, original_variant_name);
        self.full_names
            .borrow_mut()
            .insert(original_variant_name.to_owned(), name);
        None
    }

    fn process_comment(&self, comment: &str) -> Option<String> {
        let mut output = String::new();
        let mut newlines = 0;
        let mut arguments = false;

        for line in comment.split('\n') {
            let mut line = line.trim();
            if line.is_empty() {
                newlines += 1;
                continue;
            }

            let mut argument = false;
            if let Some(pline) = line.strip_prefix("@param") {
                line = pline;
                argument = true;
            }

            match newlines {
                _ if argument => output.push('\n'),
                0 => {}
                1 => output.push_str("<br>"),
                _ => output.push_str("\n\n"),
            };
            newlines = 0;

            if argument {
                if !arguments {
                    output.push_str("# Arguments\n");
                    arguments = true;
                }
                output.push_str("* ");
            }

            let line = line
                .replace('[', "\\[")
                .replace(']', "\\]")
                .replace('<', "\\<")
                .replace('>', "\\>")
                .replace("NULL", "`NULL`");
            let mut line = self.types.replace_all(&line, |c: &regex::Captures| {
                let name = &c[0];
                if name.contains('*') {
                    return format!("`{}`", name);
                }

                let full_names = self.full_names.borrow();
                if let Some(full_name) = full_names.get(name) {
                    return format!("[`{}`]({})", name, full_name);
                }

                if let Some(short_name) = name.strip_suffix("s") {
                    if let Some(full_name) = full_names.get(short_name) {
                        return format!("[`{}`]({})s", short_name, full_name);
                    }
                }

                format!("[`{}`]", name)
            });

            if let Some((first, rest)) = line.split_once(": ") {
                let mut new_line = String::new();

                for arg in first.split(", ") {
                    if arg.contains(|c: char| c.is_whitespace() || c == '`') {
                        new_line.clear();
                        break;
                    }

                    if !new_line.is_empty() {
                        new_line.push_str(", ");
                    }
                    new_line.push('`');
                    new_line.push_str(arg);
                    new_line.push('`');
                }

                if !new_line.is_empty() {
                    new_line.push_str(": ");
                    new_line.push_str(rest);
                    line = new_line.into();
                }
            }

            output.push_str(&line);

            newlines += 1;
        }
        Some(output)
    }

    fn add_derives(&self, info: &bindgen::callbacks::DeriveInfo<'_>) -> Vec<String> {
        static ZEROCOPY_TYPES: [&str; 2] = ["fz_point", "fz_quad"];

        if ZEROCOPY_TYPES.contains(&info.name) {
            [
                "zerocopy::FromBytes",
                "zerocopy::IntoBytes",
                "zerocopy::Immutable",
            ]
            .into_iter()
            .map(ToString::to_string)
            .collect()
        } else {
            vec![]
        }
    }
}

fn main() {
    if fs::read_dir("mupdf").map_or(true, |d| d.count() == 0) {
        println!("The `mupdf` directory is empty, did you forget to pull the submodules?");
        println!("Try `git submodule update --init --recursive`");
        panic!();
    }

    println!("cargo:rerun-if-changed=wrapper.h");
    println!("cargo:rerun-if-changed=wrapper.c");

    build_libmupdf();

    let mut build = cc::Build::new();
    build.file("wrapper.c").include("./mupdf/include");
    if cfg!(target_os = "android") {
        build.flag("-DHAVE_ANDROID").flag_if_supported("-std=c99");
    }
    #[cfg(target_arch = "x86_64")]
    {
        build.flag("-DARCH_HAS_SSE=1");
    }
    build.compile("libmupdf-wrapper.a");

    let bindings = bindgen::Builder::default()
        .clang_arg("-I./mupdf/include")
        .header("wrapper.h")
        .header("wrapper.c")
        .allowlist_function("fz_.*")
        .allowlist_function("pdf_.*")
        .allowlist_function("ucdn_.*")
        .allowlist_function("Memento_.*")
        .allowlist_function("mupdf_.*")
        .allowlist_type("fz_.*")
        .allowlist_type("pdf_.*")
        .allowlist_var("fz_.*")
        .allowlist_var("FZ_.*")
        .allowlist_var("pdf_.*")
        .allowlist_var("PDF_.*")
        .allowlist_var("UCDN_.*")
        .parse_callbacks(Box::new(bindgen::CargoCallbacks::new()))
        .parse_callbacks(Box::new(Callback::default()))
        .size_t_is_usize(true)
        .generate()
        .expect("Unable to generate bindings");

    // Write the bindings to the $OUT_DIR/bindings.rs file.
    let out_path = PathBuf::from(env::var("OUT_DIR").unwrap());
    bindings
        .write_to_file(out_path.join("bindings.rs"))
        .expect("Couldn't write bindings!");
}
