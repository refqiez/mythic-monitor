# Condition expression syntax

## General rules
- An expression must be **exactly one line**


## Literals
```
Boolean: true, false
Float: 3.14, 0.5, 123.456  (must contain decimal point)
```

Note that floats are decimal representation only.
No scientific notation.


## Identifier path

- Dot-separated path, consisting of case-sensitive alphanumeric + underscore identifiers.
- Identifiers may be prefixed with '$' to be parameterized.
- Leading / trailing priod is not allowed.

```text
user.age
v_config.$env.timeout
foo.$bar.baz
```


## Arithmetic operators

Binary arithmatic operators `+ - * /` and unary ops `+ -` are supported.

```text
a + b * - c
(x + y) / z
```

- Standard precedence: `* /` > `+ -`
- Unary `+`/`-` binds tighter than `* /`

## Comparison operators

```text
=   ==   !=   <   <=   >   >=
```

Standard comparison operators + chained comparison.
Had to distinguish equality operator for float and bool due to parser design;
`=` is for float, `==` is for boolean.


```text
a > b
x = y
a < b < c   # equivalent to a < b and b < c
x >= y != z # equivalent to x >= y and y != z
```


## Boolean operators

```
a > b and not is_deleted
a > b or c < d
```

- Python-style: `and`, `or`, `not`
- Operands must be boolean (no coercion, no truthy/falsy values)


## Conditional (Ternary) Expression

```
value_if_true if condition else value_if_false
Examples:
x if x > 0 else -x
"is_adult" if age >= 18 else "minor"
```

- Condition must be boolean
- Both branches must have the same type
- Lowest precedence operator


## Operator Precedence (Highest → Lowest)
1. Parentheses `( ... )`
2. Unary: `not`, `+`, `-`
3. Arithmetic: `* /`
4. Arithmetic: `+ -`
5. Comparison: `== != < <= > >=`
6. Boolean: `and`
7. Boolean: `or`
8. Conditional: `... if ... else ...`

## Example Expressions
```text
-(a + b) * c > 10 and not is_deleted
1 <= x < 10
x if flag else y
not -x + +y
a / (b - b)   # runtime error: division by zero
```


## Takeaway
- Expressions read like math + Python
- Precedence works implicitly; use parentheses for clarity
- Strictly typed in boolean and number types
- Unary `not`, `-`, `+` supported
- No hidden coercions or side effects