# SPDX-License-Identifier: Apache-2.0
# Run: python -I gen_cases.py cases.txt
# The Calculator's 500 host sums (TASK-112), each with its answer worked out
# independently: exact fractions for arithmetic, Python's decimal module at
# 90 digits for the functions. Written to lib/fdecimal/src/tests/cases.txt as
#   mode <TAB> sum <TAB> answer        (answer "!Error" for the errors)
import random, sys
sys.set_int_max_str_digits(0)
from fractions import Fraction as F
from decimal import Decimal as D, getcontext, localcontext

getcontext().prec = 90
SHOWN = 32
random.seed(20261119)

def pi90():
    with localcontext() as c:
        c.prec = 100
        lasts, t, s, n, na, d, da = 0, D(3), 3, 1, 0, 0, 24
        while s != lasts:
            lasts = s
            n, na = n + na, na + 8
            d, da = d + da, da + 32
            t = (t * n) / d
            s += t
    return +s

PI = pi90()
E = D(1).exp()

def dsin(x):
    with localcontext() as c:
        c.prec = 110
        x = x % (2 * PI)
        i, lasts, s, fact, num, sign = 1, 0, x, 1, x, 1
        while s != lasts:
            lasts = s
            i += 2
            fact *= i * (i - 1)
            num *= x * x
            sign *= -1
            s += num / fact * sign
    return +s

def dcos(x):
    with localcontext() as c:
        c.prec = 110
        x = x % (2 * PI)
        i, lasts, s, fact, num, sign = 0, 0, D(1), 1, D(1), 1
        while s != lasts:
            lasts = s
            i += 2
            fact *= i * (i - 1)
            num *= x * x
            sign *= -1
            s += num / fact * sign
    return +s

def datan(x):
    with localcontext() as c:
        c.prec = 110
        k = 0
        while abs(x) > D('0.05'):
            x = x / (1 + (1 + x * x).sqrt())
            k += 1
        s, p, n, lasts = x, x, 1, None
        while s != lasts:
            lasts = s
            p *= -x * x
            s += p / (2 * n + 1)
            n += 1
        s = s * (2 ** k)
    return +s

def dasin(x):
    if x == 1: return PI / 2
    if x == -1: return -PI / 2
    return datan(x / (1 - x * x).sqrt())

def fmt(q):
    if q == 0:
        return "0"
    neg, a = q < 0, abs(q)
    e = len(str(a.numerator)) - len(str(a.denominator))
    while F(10) ** e > a: e -= 1
    while F(10) ** (e + 1) <= a: e += 1
    n = round(a * F(10) ** (SHOWN - 1 - e))  # half to even
    if n == 10 ** SHOWN:
        n //= 10
        e += 1
    if e > 9999: return "!Overflow"
    if e < -9999: return "0"
    digits, s = str(n), "-" if neg else ""
    if -7 <= e < SHOWN:
        point = e + 1
        if point <= 0: body = "0." + "0" * (-point) + digits
        elif point >= len(digits): body = digits + "0" * (point - len(digits))
        else: body = digits[:point] + "." + digits[point:]
        if "." in body: body = body.rstrip("0").rstrip(".")
        return s + body
    sig = digits.rstrip("0") or "0"
    return s + sig[0] + ("." + sig[1:] if len(sig) > 1 else "") + "e" + ("-" if e < 0 else "+") + str(abs(e))

def rnum(maxint=6, maxfrac=6, zero_ok=True):
    while True:
        di, df = random.randint(0, maxint), random.randint(0, maxfrac)
        ip = str(random.randint(0, 10 ** di - 1)) if di else "0"
        s = ip + ("." + "".join(random.choice("0123456789") for _ in range(df)) if df else "")
        if zero_ok or F(s) != 0:
            return s

cases = []
def add(mode, text, value):
    cases.append((mode, text, value if isinstance(value, str) else fmt(value)))

# 1. Flat chains, the usual order (Python's own parser keeps the score): 180.
for _ in range(180):
    k = random.randint(2, 5)
    calc, py = [], []
    for i in range(k):
        n = rnum(zero_ok=True)
        if random.random() < 0.08:
            n = n + "E" + str(random.randint(-12, 12))
        if i > 0:
            op = random.choice("+-×÷*/")
            if op in "÷/":
                n = rnum(zero_ok=False)
            calc.append(op)
            py.append({"×": "*", "÷": "/"}.get(op, op))
        neg = (i == 0 and random.random() < 0.2) or (i > 0 and random.random() < 0.05)
        calc.append(("-" if neg else "") + n)
        py.append(("-" if neg else "") + "F('" + n.lower() + "')")
    add("rad", "".join(calc), eval("".join(py)))

# 2. Brackets, nested: 60.
def tree(depth):
    if depth == 0 or random.random() < 0.25:
        n = rnum(4, 4, zero_ok=False)
        return n, F(n)
    (a, va), (b, vb) = tree(depth - 1), tree(depth - 1)
    op = random.choice("+-×÷")
    v = {"+": va + vb, "-": va - vb, "×": va * vb, "÷": va / vb if vb else None}[op]
    if v is None:
        return tree(depth)
    return "(" + a + op + b + ")", v
for _ in range(60):
    t, v = tree(3)
    add("rad", t, v)

# 3. Powers: whole (exact) and not: 60.
for _ in range(40):
    b, n = rnum(2, 3, zero_ok=False), random.randint(-8, 30)
    neg = random.random() < 0.25
    text = ("(-" + b + ")" if neg else b) + "^" + str(n)
    add("rad", text, (-F(b) if neg else F(b)) ** n)
for _ in range(15):
    b, y = rnum(2, 3, zero_ok=False), rnum(1, 2, zero_ok=False)
    if F(y).denominator == 1:
        y = y + ".5" if "." not in y else y + "5"
    add("rad", b + "^" + y, F(D(b) ** D(y)))
for b, n in [("7", 500), ("2", 1000), ("1.5", 200), ("0.5", 100), ("10", 9999)]:
    add("rad", b + "^" + str(n), F(b) ** n)

# 4. Square roots: 40 (some exact).
for _ in range(30):
    x = rnum(5, 4, zero_ok=False)
    add("rad", "√" + ("(" + x + ")" if random.random() < 0.5 else x), F(D(x).sqrt()))
for r in ["1.2", "12", "0.05", "1.1", "300", "0.0007", "99", "2.25", "1234.5", "0.3"]:
    x = str(F(r) ** 2) if F(r).denominator == 1 else str(D(r) ** 2)
    add("rad", "sqrt(" + x + ")", F(r))

# 5. ln, log, exp: 45.
while sum(1 for c in cases if c[1].startswith("ln(")) < 15:
    x = rnum(6, 4, zero_ok=False)
    if abs(D(x).ln()) > D("0.001"):
        add("rad", "ln(" + x + ")", F(D(x).ln()))
for _ in range(10):
    x = rnum(6, 4, zero_ok=False)
    if abs(D(x).log10()) > D("0.001"):
        add("rad", "log(" + x + ")", F(D(x).log10()))
for k in [3, -3, 0, 12, -9]:
    x = str(F(10) ** k) if k >= 0 else "0." + "0" * (-k - 1) + "1"
    add("rad", "log(" + x + ")", F(k))
for _ in range(15):
    x = ("-" if random.random() < 0.4 else "") + rnum(2, 4)
    add("rad", "exp(" + x + ")", F(D(x).exp()))

# 6. Trigonometry, radians and degrees, and back: 60.
def keep(v):
    return abs(v) > D("0.001")
n = 0
while n < 15:
    x = ("-" if random.random() < 0.3 else "") + rnum(1, 4)
    f = random.choice(["sin", "cos", "tan"])
    v = {"sin": dsin, "cos": dcos, "tan": lambda t: dsin(t) / dcos(t)}[f](D(x))
    if keep(v) and abs(v) < 1000:
        add("rad", f + "(" + x + ")", F(v)); n += 1
for x in ["30", "45", "60", "90", "120", "135", "150", "180", "210", "270", "-30", "390", "720", "-90", "315"]:
    f = random.choice(["sin", "cos"])
    r = D(x) * PI / 180
    v = dsin(r) if f == "sin" else dcos(r)
    add("deg", f + "(" + x + ")", F(v) if abs(v) > D("1e-60") else F(0))
add("deg", "tan(45)", F(1))
add("deg", "tan(90)", "!Domain")
n = 0
while n < 10:
    x = ("-" if random.random() < 0.3 else "") + rnum(3, 3)
    f = random.choice(["sin", "cos", "tan"])
    r = D(x) * PI / 180
    v = {"sin": dsin, "cos": dcos, "tan": lambda t: dsin(t) / dcos(t)}[f](r)
    if keep(v) and abs(v) < 1000:
        add("deg", f + "(" + x + ")", F(v)); n += 1
n = 0
while n < 18:
    mode = random.choice(["rad", "deg"])
    f = random.choice(["asin", "acos", "atan"])
    x = ("-" if random.random() < 0.3 else "") + (("0." + str(random.randint(1, 999)).zfill(3)) if f != "atan" else rnum(2, 3))
    if f != "atan" and abs(D(x)) > 1:
        continue
    v = {"asin": dasin, "acos": lambda t: PI / 2 - dasin(t), "atan": datan}[f](D(x))
    if mode == "deg":
        v = v * 180 / PI
    if keep(v):
        add(mode, f + "(" + x + ")", F(v)); n += 1

# 7. Factorials and percent: 25.
for k in [0, 1, 2, 5, 10, 13, 20, 25, 30, 50, 69, 100, 170, 300, 1000]:
    import math
    add("rad", str(k) + "!", F(math.factorial(k)))
for text, v in [("50%", F(1, 2)), ("200×15%", F(30)), ("12.5%", F(1, 8)), ("3!!", F(720)), ("(2+3)!", F(120)),
                ("0.5%", F(1, 200)), ("1000×0.1%", F(1)), ("4!÷3!", F(4)), ("10%×10%", F(1, 100)), ("7%+3%", F(1, 10))]:
    add("rad", text, v)

# 8. What has no answer: 20.
for text, err in [("1÷0", "!DivideByZero"), ("5/(3-3)", "!DivideByZero"), ("√(-4)", "!Domain"), ("ln(0)", "!Domain"),
                  ("ln(-1)", "!Domain"), ("log(0)", "!Domain"), ("0^-1", "!DivideByZero"), ("(-8)^0.5", "!Domain"),
                  ("3001!", "!Overflow"), ("2.5!", "!Domain"), ("(-1)!", "!Domain"), ("asin(2)", "!Domain"),
                  ("acos(-1.5)", "!Domain"), ("1E9999×100", "!Overflow"), ("2+", "!Syntax"), ("×3", "!Syntax"),
                  ("()", "!Syntax"), ("1..2", "!Syntax"), ("4)", "!Syntax"), ("foo(2)", "!Syntax")]:
    add("rad", text, err)

# 9. The constants: 10.
for text, v in [("π", F(PI)), ("e", F(E)), ("2π", F(2 * PI)), ("π^2", F(PI * PI)), ("e^2", F(E * E)),
                ("ln(e)", F(1)), ("π÷4", F(PI / 4)), ("sin(π÷6)", F(dsin(PI / 6))), ("cos(π)", F(-1)), ("3(4+1)", F(15))]:
    add("rad", text, v)

if len(cases) != 500:
    sys.exit("cases: %d, not 500" % len(cases))
with open(sys.argv[1], "w", encoding="utf-8", newline="\n") as f:
    f.write("# The Calculator's 500 host sums (TASK-112): mode, sum, answer (gen_cases.py).\n")
    for m, t, v in cases:
        f.write(m + "\t" + t + "\t" + v + "\n")
print("wrote", len(cases))
