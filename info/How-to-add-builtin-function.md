# how to add a builtin function

### Conventions

Naming:

* Use `snake_case`, and one name per function: do not add synonyms (`send_email` and `smtp_send` are only kept as
  deprecated aliases of `send_mail`). Put an optional argument at the end instead of adding a variant,
  e.g. `send_mail(from, to, subject, body[, smtp_url])`.
* The `_` prefix is only for functions computing a value from the values of an array: `_sum`, `_mean`, `_median`,
  `_top`, `_join` ... Other functions taking or filling arrays keep plain names: gawk functions (`asort`, `split`,
  `length`), generators (`seq`, `parse_array`) and transforms (`uniq`).
* Group related functions by a prefix: `kv_*`, `json_*`, `sqlite_*`, `jev_*`.

Errors:

* A failure returns the zero value of the return type: `""` for a string, `0` for a number, an empty array for a map,
  and nothing for `void`; the script keeps running.
* Report the reason with `crate::runtime::stdlib_warning(function_name, message)`, which prints
  `zawk: warning: function_name: message` to stderr once per distinct message. Do not use `eprintln!` or `panic!`.
* Exceptions only come from awk/gawk compatibility: `close()`, `getline` and `mktime()` return `-1`, `system()` returns
  the exit status. A status inside a returned value is part of the value, e.g. `http_get(url)["status"]` is `0` on a
  network error.

### Declare function

* Declare function name in `pub enum Function ` of [src/builtins.rs](../src/builtins.rs)
* Bind function name in `static_map!` of [src/builtins.rs](../src/builtins.rs)
* Add function name in `pub(crate) fn arity(`(参数个数) function of [src/builtins.rs](../src/builtins.rs)
* Add function name in `fn type_sig(`(函数类型签名) of [src/builtins.rs](../src/builtins.rs)
* Add function name in `fn step(`(函数返回值类型) of [src/builtins.rs](../src/builtins.rs)
* Dynamic parameters in `fn call<'c>(` of [src/cfg.rs](../src/cfg.rs)

### Bind function with AWK compiler

* add display for `impl Display for Function`(字符串化) of [src/display.rs](../src/display.rs)
* Add function name in `fn builtin(`(内置指令集) of [src/compile.rs](../src/compile.rs)
* Add function signature in `pub(crate) enum Instr`(指令签名) of [src/bytecode.rs](../src/bytecode.rs)
* Add function name in `pub(crate) fn accum(`(清空器) of [src/bytecode.rs](../src/bytecode.rs)
* Add function name in `fn visit_ll(`(遍历器) of [src/dataflow.rs](../src/dataflow.rs)
* Add function name in `fn run_at(`(并发运行) of [src/interp.rs](../src/interp.rs) 并发时调用

### Function implementation

* Add function name in `fn gen_ll_inst` of [src/codegen/mod.rs](../src/codegen/mod.rs)
* register function in `register! {`(指令编译注册) of [src/codegen/intrinsics.rs](../src/codegen/intrinsics.rs)
* Add function implementation: 可以考虑将功能实现放到runtime下的一个module中，如`date_time.rs`。

`uuid`(generator) implementation in [src/codegen/intrinsics.rs](../src/codegen/intrinsics.rs)

```
pub(crate) unsafe extern "C" fn uuid(runtime: *mut c_void) -> U128 {
    let res = Str::from("demo");
    mem::transmute::<Str, U128>(res)
}
```

`fend`(transform) implementation in [src/runtime/str_impl.rs](../src/runtime/str_impl.rs)

```
    pub fn fend<'b>(&self) -> Str<'b> {
        let mut context = fend_core::Context::new();
        let expr = self.to_string();
        return match fend_core::evaluate(&expr, &mut context) {
            Ok(result) => {Str::from(result.get_main_result().to_string())}
            Err(error) => {
                crate::runtime::stdlib_warning("fend", format!("{:?}: {}", expr, error));
                Str::default()
            }
        }
    }
```

If you can not determine param type, such as min(param1, param2), please use `Str`.

### Dynamic Param Type

* Please check param type in `fn builtin(`(内置指令集) of [src/compile.rs](../src/compile.rs)  and match function
  signature in `pub(crate) enum Instr`(指令签名) of [src/bytecode.rs](../src/bytecode.rs).

### Custom Math function

* Declare function name in `pub enum FloatFunc ` of [src/builtins.rs](../src/builtins.rs)
* Implement relative logic in [src/builtins.rs](../src/builtins.rs)
* register function in `register! {` of [src/codegen/intrinsics.rs](../src/codegen/intrinsics.rs)
* Declare FFI function `pub(crate) unsafe extern "C" fn _frawk_abs`
  of [src/codegen/intrinsics.rs](../src/codegen/intrinsics.rs)
* Add logic in `fn floatfunc(` of [src/codegen/clif.rs](../src/codegen/clif.rs)

### Resource functions

* File: getline https://www.gnu.org/software/gawk/manual/html_node/Getline.html

### Global variables

please refer `ENVIRON` as example.

* Declare variable name in `pub(crate) enum Variable {` of [src/builtins.rs](../src/builtins.rs)
* Change some logic in `impl From<Variable> for compile::Ty` of [src/builtins.rs](../src/builtins.rs)
* Implement logic in `fn default() -> Variables<'a>` of [src/builtins.rs](../src/builtins.rs)
* register variable `static_map!(` of [src/builtins.rs](../src/builtins.rs)
* add display in `impl Display for Variable`(字符串化) of [src/display.rs](../src/display.rs)
* Add variable support in `pub fn shuttle(&self, pid: Int)` of [src/interp.rs](../src/interp.rs)

### File format support

* text
* csv
* Apache Parquet - use dr to convert Parquet to CSV

### References

* inflector: add String based inflections for Rust. Snake, kebab, train, camel, sentence, class, and title
  cases - https://github.com/whatisinternet/inflector
* Internationalization with gawk: https://www.gnu.org/software/gawk/manual/html_node/I18N-Example.html

# Tools

* Apache Parquet Read: please use [dr](https://crates.io/crates/dr) to convert parquet to CSV file.
