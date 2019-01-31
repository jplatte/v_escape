use std::{fmt::{Display, Write}, str};

use crate::parser::Pair;

struct Generator<'a> {
    pairs: &'a [Pair<'a>],
    simd: bool,
    ranges: bool,
    avx: bool,
}

type Ranges = Vec<u8>;

pub fn generate(pairs: &[Pair], simd: bool, ranges: bool, avx: bool) -> String {
    Generator::new(pairs, simd, ranges, avx).build()
}

// End flag for indicate more escapes than ranges
const FLAG: u8 = 128;

impl<'a> Generator<'a> {
    pub fn new<'n>(pairs: &'n [Pair<'n>], simd: bool, ranges: bool, avx: bool) -> Generator<'n> {
        Generator {
            pairs,
            simd,
            ranges,
            avx,
        }
    }

    pub fn build(&self) -> String {
        let mut buf = Buffer::new(0);

        self.write_static_table(&mut buf);
        self.write_functions(&mut buf);
        self.write_cfg_if(&mut buf);

        buf.buf
    }

    fn write_static_table(&self, buf: &mut Buffer) {
        let len = self.pairs.len();

        buf.write("static V_ESCAPE_TABLE: [u8; 256] = [");
        for i in 0..=255 as u8 {
            let n = self
                .pairs
                .binary_search_by(|s| s.char.cmp(&i))
                .unwrap_or(len);
            buf.write(&format!("{}, ", n))
        }
        buf.writeln("];");

        let quotes: Vec<&str> = self
            .pairs
            .iter()
            .map(|s| str::from_utf8(s.quote).expect("valid utf-8 quote"))
            .collect();
        buf.writeln(&format!(
            "static V_ESCAPE_QUOTES: [&str; {}] = {:#?};",
            len, quotes
        ));

        buf.writeln(&format!("const V_ESCAPE_QUOTES_LEN: usize = {};", len));
    }

    fn write_functions(&self, buf: &mut Buffer) {
        self.write_scalar(buf);
        if self.simd {
            if self.ranges {
                self.write_ranges(buf);
            } else {
                self.write_eq(buf);
            }
        }
    }

    fn write_scalar(&self, buf: &mut Buffer) {
        let code = quote!(
            mod scalar {
                use super::*;
                _v_escape_escape_scalar!(V_ESCAPE_TABLE, V_ESCAPE_QUOTES, V_ESCAPE_QUOTES_LEN);
            }
        );
        buf.writeln(&code.to_string());
    }

    fn write_eq(&self, buf: &mut Buffer) {
        buf.writeln(r#"#[cfg(all(target_arch = "x86_64", not(v_escape_nosimd)))]"#);
        buf.writeln("mod sse {");
        buf.writeln("use super::*;");

        let mut chars: Vec<u8> = self.pairs.iter().map(|s| s.char).collect();
        assert!(chars.len() <= 16);
        let r = 16 - chars.len();
        chars.extend(vec![0; r]);
        let chars: &[u8] = &chars;

        buf.write(" _v_escape_escape_sse!((V_ESCAPE_TABLE, V_ESCAPE_QUOTES, V_ESCAPE_QUOTES_LEN) ");
        self.write_macro_tt(buf, chars);
        buf.writeln(");");

        buf.writeln("}");
    }

    fn write_ranges(&self, buf: &mut Buffer) {
        buf.writeln(r#"#[cfg(all(target_arch = "x86_64", not(v_escape_nosimd)))]"#);
        buf.writeln("mod ranges {");

        let ranges: &[u8] = &self.calculate_ranges();

        let t: &[&str] = if self.avx { &["avx", "sse"] } else { &["sse"] };

        for i in t {
            buf.write("pub mod ");
            buf.write(i);
            buf.writeln(" {");
            buf.writeln("use super::super::*;");
            buf.write("_v_escape_escape_ranges!(");
            buf.write(i);
            buf.write("2 (V_ESCAPE_TABLE, V_ESCAPE_QUOTES, V_ESCAPE_QUOTES_LEN) ");
            self.write_macro_tt(buf, ranges);
            buf.writeln(");");
            buf.writeln("}");
        }
        buf.writeln("}");
    }

    fn write_cfg_if(&self, buf: &mut Buffer) {
        buf.writeln(&format!(
            "_v_escape_cfg_escape!({}, {}, {});",
            self.simd, self.ranges, self.avx
        ));
    }

    fn write_macro_tt<T, I>(&self, buf: &mut Buffer, i: I)
    where
        T: Display,
        I: IntoIterator<Item = T>,
    {
        for c in i.into_iter() {
            buf.buf.write_fmt(format_args!("{}, ", c)).unwrap();
        }
    }

    fn calculate_ranges(&self) -> Ranges {
        let len = self.pairs.len();
        let mut ranges: Ranges = vec![];

        assert_ne!(len, 0);
        if len == 1 {
            ranges.push(self.pairs[0].char);
            ranges.push(FLAG);

            return ranges;
        }
        let len = len - 1;

        let mut d = vec![];
        for i in 0..len {
            let diff = self.pairs[i + 1].char - self.pairs[i].char;
            if 1 < diff {
                d.push((i, diff));
            }
        }
        d.sort_unstable_by(|a, b| b.1.cmp(&a.1));

        match d.len() {
            0 => {
                // 1 range
                ranges.push(self.pairs[0].char);
                ranges.push(self.pairs[len].char);
            }
            1 => {
                if len == 1 {
                    // 2 escapes
                    ranges.push(self.pairs[0].char);
                    ranges.push(self.pairs[len].char);
                    ranges.push(FLAG);
                } else {
                    let i = d[0].0;
                    if i == 0 {
                        // 1 escape and 1 range
                        ranges.push(self.pairs[i + 1].char);
                        ranges.push(self.pairs[len].char);
                        ranges.push(self.pairs[0].char);
                    } else {
                        // 1 escape and 1 range
                        ranges.push(self.pairs[0].char);
                        ranges.push(self.pairs[i].char);
                        ranges.push(self.pairs[i + 1].char);
                        if i + 1 != len {
                            // 2 ranges
                            ranges.push(self.pairs[len].char);
                        }
                    }
                }
            }
            _ => {
                if len <= 2 {
                    assert_eq!(len, 2);
                    // 3 escapes
                    for Pair { char, .. } in self.pairs {
                        ranges.push(*char);
                    }
                    ranges.push(FLAG);

                    return ranges;
                }

                let (d, _) = d.split_at_mut(2);
                d.sort_unstable_by_key(|d| d.0);

                let first = d[0].0;
                let last = d[1].0;

                if first + 1 == last {
                    if first == 0 {
                        self.push_1_ranges_2_escape_at_first(&mut ranges, first, last, len);
                    } else {
                        if last + 1 == len {
                            self.push_1_ranges_2_escape_at_last(&mut ranges, first, last, len);
                        } else {
                            self.push_2_ranges_1_escape(&mut ranges, first, last, len);
                        }
                    }
                } else {
                    if first == 0 {
                        if last + 1 == len {
                            self.push_1_ranges_2_escape_at_first_last(&mut ranges, first, last, len);
                        } else {
                            self.push_2_ranges_1_escape_at_first(&mut ranges, first, last, len);
                        }
                    } else if last + 1 == len {
                        self.push_2_ranges_1_escape_at_last(&mut ranges, first, last, len);
                    } else {
                        self.push_3_ranges(&mut ranges, first, last, len);
                    }
                }
            }
        };

        ranges
    }

    #[inline]
    fn push_1_ranges_2_escape_at_first(&self, r: &mut Ranges, first: usize, last: usize, len: usize) {
        r.push(self.pairs[last + 1].char);
        r.push(self.pairs[len].char);
        r.push(self.pairs[0].char);
        r.push(self.pairs[first + 1].char);
        r.push(FLAG);
    }

    #[inline]
    fn push_1_ranges_2_escape_at_last(&self, r: &mut Ranges, first: usize, last: usize, len: usize) {
        r.push(self.pairs[0].char);
        r.push(self.pairs[first].char);
        r.push(self.pairs[last].char);
        r.push(self.pairs[len].char);
        r.push(FLAG);
    }

    #[inline]
    fn push_1_ranges_2_escape_at_first_last(&self, r: &mut Ranges, first: usize, last: usize, len: usize) {
        r.push(self.pairs[first + 1].char);
        r.push(self.pairs[last].char);
        r.push(self.pairs[0].char);
        r.push(self.pairs[len].char);
        r.push(FLAG);
    }

    #[inline]
    fn push_2_ranges_1_escape(&self, r: &mut Ranges, first: usize, last: usize, len: usize) {
        r.push(self.pairs[0].char);
        r.push(self.pairs[first].char);
        r.push(self.pairs[last + 1].char);
        r.push(self.pairs[len].char);
        r.push(self.pairs[first + 1].char);
    }

    #[inline]
    fn push_2_ranges_1_escape_at_first(&self, r: &mut Ranges, first: usize, last: usize, len: usize) {
        r.push(self.pairs[first + 1].char);
        r.push(self.pairs[last].char);
        r.push(self.pairs[last + 1].char);
        r.push(self.pairs[len].char);
        r.push(self.pairs[0].char);
    }

    #[inline]
    fn push_2_ranges_1_escape_at_last(&self, r: &mut Ranges, first: usize, last: usize, len: usize) {
        r.push(self.pairs[0].char);
        r.push(self.pairs[first].char);
        r.push(self.pairs[first + 1].char);
        r.push(self.pairs[last].char);
        r.push(self.pairs[len].char);
    }

    #[inline]
    fn push_3_ranges(&self, r: &mut Ranges, first: usize, last: usize, len: usize) {
        r.push(self.pairs[0].char);
        r.push(self.pairs[first].char);
        r.push(self.pairs[first + 1].char);
        r.push(self.pairs[last].char);
        r.push(self.pairs[last + 1].char);
        r.push(self.pairs[len].char);
    }
}

struct Buffer {
    // The buffer to generate the code into
    buf: String,
    // The current level of indentation (in spaces)
    indent: u8,
    // Whether the output buffer is currently at the start of a line
    start: bool,
}

impl Buffer {
    fn new(indent: u8) -> Self {
        Self {
            buf: String::new(),
            indent,
            start: true,
        }
    }

    fn writeln(&mut self, s: &str) {
        if s == "}" {
            self.dedent();
        }
        if !s.is_empty() {
            self.write(s);
        }
        self.buf.push('\n');
        if s.ends_with('{') {
            self.indent();
        }
        self.start = true;
    }

    fn write(&mut self, s: &str) {
        if self.start {
            for _ in 0..(self.indent * 4) {
                self.buf.push(' ');
            }
            self.start = false;
        }
        self.buf.push_str(s);
    }

    fn indent(&mut self) {
        self.indent += 1;
    }

    fn dedent(&mut self) {
        if self.indent == 0 {
            panic!("dedent() called while indentation == 0");
        }
        self.indent -= 1;
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::parser::Pair;

    static E: &[u8] = b"f";

    #[test]
    fn test_1_escape() {
        let pairs = &[Pair::new(0, E)];
        let g = Generator::new(pairs, false, false, false);

        assert_eq!(g.calculate_ranges(), vec![0, 128])
    }

    #[test]
    fn test_2_escape() {
        let pairs = &[Pair::new(0, E), Pair::new(2, E)];
        let g = Generator::new(pairs, false, false, false);

        assert_eq!(g.calculate_ranges(), vec![0, 2, 128])
    }

    #[test]
    fn test_3_escape() {
        let pairs = &[Pair::new(0, E), Pair::new(2, E), Pair::new(4, E)];
        let g = Generator::new(pairs, false, false, false);

        assert_eq!(g.calculate_ranges(), vec![0, 2, 4, 128])
    }

    #[test]
    fn test_1_range() {
        let pairs = &[Pair::new(0, E), Pair::new(1, E)];
        let g = Generator::new(pairs, false, false, false);

        assert_eq!(g.calculate_ranges(), vec![0, 1])
    }

    #[test]
    fn test_2_range() {
        let pairs = &[
            Pair::new(0, E),
            Pair::new(1, E),
            Pair::new(3, E),
            Pair::new(4, E),
        ];
        let g = Generator::new(pairs, false, false, false);

        assert_eq!(g.calculate_ranges(), vec![0, 1, 3, 4])
    }

    #[test]
    fn test_3_range() {
        let pairs = &[
            Pair::new(0, E),
            Pair::new(1, E),
            Pair::new(3, E),
            Pair::new(4, E),
            Pair::new(6, E),
            Pair::new(7, E),
        ];
        let g = Generator::new(pairs, false, false, false);

        assert_eq!(g.calculate_ranges(), vec![0, 1, 3, 4, 6, 7]);
        let pairs = &[
            Pair::new(0, E),
            Pair::new(1, E),
            Pair::new(3, E),
            Pair::new(5, E),
            Pair::new(7, E),
            Pair::new(9, E),
            Pair::new(50, E),
            Pair::new(52, E),
            Pair::new(55, E),
            Pair::new(60, E),
            Pair::new(61, E),
            Pair::new(62, E),
            Pair::new(63, E),
            Pair::new(64, E),
            Pair::new(126, E),
            Pair::new(127, E),
        ];
        let g = Generator::new(pairs, false, false, false);

        assert_eq!(g.calculate_ranges(), vec![0, 9, 50, 64, 126, 127])
    }

    #[test]
    fn test_1_range_1_escape() {
        let pairs = &[Pair::new(0, E), Pair::new(1, E), Pair::new(3, E)];
        let g = Generator::new(pairs, false, false, false);

        assert_eq!(g.calculate_ranges(), vec![0, 1, 3]);

        let pairs = &[Pair::new(0, E), Pair::new(2, E), Pair::new(3, E)];
        let g = Generator::new(pairs, false, false, false);

        assert_eq!(g.calculate_ranges(), vec![2, 3, 0]);

        let pairs = &[
            Pair::new(0, E),
            Pair::new(1, E),
            Pair::new(2, E),
            Pair::new(4, E),
        ];
        let g = Generator::new(pairs, false, false, false);

        assert_eq!(g.calculate_ranges(), vec![0, 2, 4]);

        let pairs = &[
            Pair::new(50, E),
            Pair::new(51, E),
            Pair::new(52, E),
            Pair::new(53, E),
            Pair::new(54, E),
            Pair::new(55, E),
            Pair::new(67, E),
        ];
        let g = Generator::new(pairs, false, false, false);

        assert_eq!(g.calculate_ranges(), vec![50, 55, 67]);
    }

    #[test]
    fn test_2_range_1_escape_a() {
        let pairs = &[
            Pair::new(0, E),
            Pair::new(1, E),
            Pair::new(3, E),
            Pair::new(4, E),
            Pair::new(6, E),
        ];
        let g = Generator::new(pairs, false, false, false);

        assert_eq!(g.calculate_ranges(), vec![0, 1, 3, 4, 6]);
    }

    #[test]
    fn test_2_range_1_escape_b() {
        let pairs = &[
            Pair::new(0, E),
            Pair::new(4, E),
            Pair::new(5, E),
            Pair::new(7, E),
            Pair::new(8, E),
        ];
        let g = Generator::new(pairs, false, false, false);

        assert_eq!(g.calculate_ranges(), vec![4, 5, 7, 8, 0]);
    }

    #[test]
    fn test_2_range_1_escape_c() {
        let pairs = &[
            Pair::new(14, E),
            Pair::new(15, E),
            Pair::new(16, E),
            Pair::new(50, E),
            Pair::new(51, E),
            Pair::new(52, E),
            Pair::new(98, E),
        ];
        let g = Generator::new(pairs, false, false, false);

        assert_eq!(g.calculate_ranges(), vec![14, 16, 50, 52, 98]);
    }

    #[test]
    fn test_2_range_1_escape_d() {
        let pairs = &[
            Pair::new(14, E),
            Pair::new(15, E),
            Pair::new(16, E),
            Pair::new(50, E),
            Pair::new(51, E),
            Pair::new(52, E),
            Pair::new(98, E),
        ];

        let g = Generator::new(pairs, false, false, false);

        assert_eq!(g.calculate_ranges(), vec![14, 16, 50, 52, 98]);
        let pairs = &[
            Pair::new(14, E),
            Pair::new(15, E),
            Pair::new(16, E),
            Pair::new(17, E),
            Pair::new(18, E),
            Pair::new(19, E),
            Pair::new(50, E),
            Pair::new(51, E),
            Pair::new(52, E),
            Pair::new(53, E),
            Pair::new(54, E),
            Pair::new(55, E),
            Pair::new(56, E),
            Pair::new(57, E),
            Pair::new(58, E),
            Pair::new(98, E),
        ];
        let g = Generator::new(pairs, false, false, false);

        assert_eq!(g.calculate_ranges(), vec![14, 19, 50, 58, 98]);
    }

    #[test]
    fn test_2_range_1_escape_e() {
        let pairs = &[
            Pair::new(14, E),
            Pair::new(16, E),
            Pair::new(50, E),
            Pair::new(51, E),
            Pair::new(52, E),
            Pair::new(98, E),
        ];

        let g = Generator::new(pairs, false, false, false);

        assert_eq!(g.calculate_ranges(), vec![14, 16, 50, 52, 98]);
        let pairs = &[
            Pair::new(14, E),
            Pair::new(16, E),
            Pair::new(17, E),
            Pair::new(18, E),
            Pair::new(19, E),
            Pair::new(50, E),
            Pair::new(52, E),
            Pair::new(53, E),
            Pair::new(56, E),
            Pair::new(57, E),
            Pair::new(58, E),
            Pair::new(98, E),
        ];
        let g = Generator::new(pairs, false, false, false);

        assert_eq!(g.calculate_ranges(), vec![14, 19, 50, 58, 98]);
    }

    #[test]
    fn test_2_range_1_escape_f() {
        let pairs = &[
            Pair::new(60, E),
            Pair::new(61, E),
            Pair::new(65, E),
            Pair::new(80, E),
            Pair::new(81, E),
        ];

        let g = Generator::new(pairs, false, false, false);

        assert_eq!(g.calculate_ranges(), vec![60, 61, 80, 81, 65]);

        let pairs = &[
            Pair::new(52, E),
            Pair::new(53, E),
            Pair::new(56, E),
            Pair::new(58, E),
            Pair::new(60, E),
            Pair::new(61, E),
            Pair::new(62, E),
            Pair::new(80, E),
            Pair::new(101, E),
            Pair::new(102, E),
            Pair::new(103, E),
            Pair::new(104, E),
            Pair::new(105, E),
            Pair::new(108, E),
            Pair::new(110, E),
            Pair::new(120, E),
        ];

        let g = Generator::new(pairs, false, false, false);

        assert_eq!(g.calculate_ranges(), vec![52, 62, 101, 120, 80]);
    }

    #[test]
    fn test_1_range_2_escape_a() {
        let pairs = &[
            Pair::new(0, E),
            Pair::new(1, E),
            Pair::new(4, E),
            Pair::new(6, E),
        ];
        let g = Generator::new(pairs, false, false, false);

        assert_eq!(g.calculate_ranges(), vec![0, 1, 4, 6, 128]);

        let pairs = &[
            Pair::new(0, E),
            Pair::new(1, E),
            Pair::new(2, E),
            Pair::new(3, E),
            Pair::new(4, E),
            Pair::new(5, E),
            Pair::new(6, E),
            Pair::new(7, E),
            Pair::new(8, E),
            Pair::new(9, E),
            Pair::new(10, E),
            Pair::new(11, E),
            Pair::new(12, E),
            Pair::new(13, E),
            Pair::new(14, E),
            Pair::new(73, E),
            Pair::new(127, E),
        ];
        let g = Generator::new(pairs, false, false, false);

        assert_eq!(g.calculate_ranges(), vec![0, 14, 73, 127, 128]);
    }

    #[test]
    fn test_1_range_2_escape_b() {
        let pairs = &[
            Pair::new(0, E),
            Pair::new(2, E),
            Pair::new(5, E),
            Pair::new(6, E),
        ];
        let g = Generator::new(pairs, false, false, false);

        assert_eq!(g.calculate_ranges(), vec![5, 6, 0, 2, 128]);

        let pairs = &[
            Pair::new(0, E),
            Pair::new(2, E),
            Pair::new(5, E),
            Pair::new(6, E),
            Pair::new(7, E),
            Pair::new(8, E),
            Pair::new(9, E),
            Pair::new(10, E),
            Pair::new(11, E),
            Pair::new(12, E),
            Pair::new(13, E),
            Pair::new(14, E),
            Pair::new(15, E),
            Pair::new(16, E),
            Pair::new(17, E),
            Pair::new(18, E),
        ];
        let g = Generator::new(pairs, false, false, false);

        assert_eq!(g.calculate_ranges(), vec![5, 18, 0, 2, 128]);
    }

    #[test]
    fn test_1_range_2_escape_c() {
        let pairs = &[
            Pair::new(0, E),
            Pair::new(2, E),
            Pair::new(3, E),
            Pair::new(8, E),
        ];
        let g = Generator::new(pairs, false, false, false);

        assert_eq!(g.calculate_ranges(), vec![2, 3, 0, 8, 128]);

        let pairs = &[
            Pair::new(0, E),
            Pair::new(2, E),
            Pair::new(3, E),
            Pair::new(4, E),
            Pair::new(5, E),
            Pair::new(6, E),
            Pair::new(7, E),
            Pair::new(8, E),
            Pair::new(9, E),
            Pair::new(10, E),
            Pair::new(11, E),
            Pair::new(12, E),
            Pair::new(13, E),
            Pair::new(14, E),
            Pair::new(15, E),
            Pair::new(16, E),
            Pair::new(17, E),
            Pair::new(127, E),
        ];
        let g = Generator::new(pairs, false, false, false);

        assert_eq!(g.calculate_ranges(), vec![2, 17, 0, 127, 128]);
    }
}
