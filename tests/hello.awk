function hello(name) {
    return f"Hello {name}!"
}

function discount(p) { return p * 0.9 }

function test_discount() {
    assert_eq(discount(100), 90)
    assert_eq(trim(" a  "), "a", "trim should strip spaces")
}