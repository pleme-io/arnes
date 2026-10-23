Sure! Here's a neat little calculus exercise and its solution:

**Problem:**
Evaluate the definite integral
\[
\int_{0}^{1} \ln(1+x)\,dx.
\]

**Step-by-step Solution**

1. **Find an antiderivative of \(\ln(1+x)\).**
   Use integration by parts:
   \[
   \int \ln(1+x)\,dx = (1+x)\ln(1+x) - \int \frac{1+x}{1+x}\,dx = (1+x)\ln(1+x) - x + C.
   \]

2. **Apply the limits \(x=0\) to \(x=1\).**
   \[
   \begin{aligned}
   F(1) &= (1+1)\ln(1+1) - 1 \\
        &= 2\ln 2 - 1, \\
   F(0) &= (1+0)\ln(1+0) - 0 = 1\cdot \ln 1 = 0.
   \end{aligned}
   \]

3. **Subtract:**
   \[
   \int_{0}^{1} \ln(1+x)\,dx = F(1)-F(0) = 2\ln 2 - 1.
   \]

**Result**
\[
\boxed{\displaystyle \int_{0}^{1} \ln(1+x)\,dx = 2\ln 2 - 1 \approx 0.386294}
\]

So the area under the curve \(y=\ln(1+x)\) from \(x=0\) to \(x=1\) is \(2\ln 2 - 1\). If you'd like to see another type of problem, maybe a series sum, a geometry puzzle, or something else, just let me know!

| method | result |
|---|---|
| by parts | 2 ln 2 - 1 |
| numeric | 0.386294 |

```rust
fn main() { println!("{}", 2.0_f64.ln() * 2.0 - 1.0); }
```

> A blockquote about integrals.

- a bullet item that is long enough that it will need to wrap onto a second line at this width
- second
