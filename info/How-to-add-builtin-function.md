# how to add a builtin function

### Declare function

* Declare function name in `pub enum Function ` of [src/builtins.rs](../src/builtins.rs)
* Bind function name in `static_map!` of [src/builtins.rs](../src/builtins.rs)
* Add function name in `pub(crate) fn arity(`(参数个数) function of [src/builtins.rs](../src/builtins.rs)
* Add function name in `fn type_sig(`(函数类型签名) of [src/builtins.rs](../src/builtins.rs)
* Add function name in `fn step(`(函数返回值类型) of [src/builtins.rs](../src/builtins.rs)
* Dynamic parameters in `fn call<'c>(` of [src/cfg.rs](../src/cfg.rs)

### Bind function with AWK compiler

* add display for `impl Display for Function`(字符串化) of [src/display.rs](../src/display.rs)
* Add function name in `fn builtin(`(指令集) of [src/compile.rs](../src/compile.rs)
* Add function name in `pub(crate) fn accum(`(清空器) of [src/bytecode.rs](../src/bytecode.rs)
* Add function name in `fn visit_ll(`(遍历器) of [src/dataflow.rs](../src/dataflow.rs)
* Add function name in `fn run_at(` of [src/interp.rs](../src/interp.rs) 重点看一下这个函数
* register function in `register! {` of [src/codegen/intrinsics.rs](../src/codegen/intrinsics.rs)

### Function implementation

* Add function name in `fn gen_ll_inst` of [src/codegen/mod.rs](../src/codegen/mod.rs)
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
            Err(error) => {Str::from(format!("FendError:{}",error))}
        }
    }
```

If you can not determine param type, such as min(param1, param2), please use `Str`.

### Custom Math function

* Declare function name in `pub enum FloatFunc ` of [src/builtins.rs](../src/builtins.rs)
* Implement relative logic in [src/builtins.rs](../src/builtins.rs)
* register function in `register! {` of [src/codegen/intrinsics.rs](../src/codegen/intrinsics.rs)
* Declare FFI function `pub(crate) unsafe extern "C" fn _frawk_abs`
  of [src/codegen/intrinsics.rs](../src/codegen/intrinsics.rs)
* Add logic in `fn floatfunc(` of [src/codegen/clif.rs](../src/codegen/clif.rs)


### Global variables

please refer `ENVIRON` as example.

* Declare variable name in `pub(crate) enum Variable {` of [src/builtins.rs](../src/builtins.rs)
* Change some logic in `impl From<Variable> for compile::Ty` of [src/builtins.rs](../src/builtins.rs)
* Implement logic in `fn default() -> Variables<'a>` of [src/builtins.rs](../src/builtins.rs)
* register variable `static_map!(` of [src/builtins.rs](../src/builtins.rs)
* add display in `impl Display for Variable`(字符串化) of [src/display.rs](../src/display.rs)
* Add variable support in `pub fn shuttle(&self, pid: Int)` of [src/interp.rs](../src/interp.rs)

### UDF(User Defined Function)

* uuid
* math: abs, floor, ceiling, round, fend("1+2"), min(1,2), max("A","B")
* string:  Please regex express for `is_xxx()`、`contains()`、`start_with()`、`end_with()` functions.
   - trim: `trim($1)` or `trim($1, "[]()")` 
   - truncate: `truncate($1, 10)` or `truncate($1, 10, "...")`
   - escape: `escape("sql", $1)`, such as json, csv,tsv, xml, html, sql.
* array: sort, sorti
* json: json_parse(text), json_stringify(array)
* encoding: `hex`, `base64`, `base64url`, `url`, `hex-base64`,`hex-base64url`, `base64-hex`,`base64url-hex`, such
  as `encode("base64", $1)`, `encode("url",$1)`, `decode("base64", $1)`, `encode("hex-base64",$1)`
* Digest: `md5`, `sha256`, `sha512`, `bcrypt`, `murmur3`, such as `digest("md5",$1)`, `digest("sha256",$1)` 
* crypto: `hmac("HmacSHA256","your-secret-key", $1)` or `hmac("HmacSHA512","your-secret-key", $1)`
* parser: `url("http://example.com/demo?query=1")`
* date time: utc by default
    - systime: current Unix time
    - strftime: https://docs.rs/chrono/latest/chrono/format/strftime/index.html
    - mktime https://docs.rs/dateparser/latest/dateparser/#accepted-date-formats  