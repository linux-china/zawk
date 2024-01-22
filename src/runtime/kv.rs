pub(crate) fn kv_get(namespace: &str, key: &str) -> String {
    "value".to_owned()
}

pub(crate) fn kv_put(namespace: &str, key: &str, value: &str) {
    println!("put {} {} {}", namespace, key, value);
}

pub(crate) fn kv_delete(namespace: &str, key: &str) {
    println!("delete {} {}", namespace, key);
}

pub(crate) fn kv_clear(namespace: &str) {
    println!("clear {}", namespace);
}
