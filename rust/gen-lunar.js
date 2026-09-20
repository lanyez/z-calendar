'use strict';
// 一次性数据生成器：用 lunar-javascript 生成 1900-2100 农历/节气数据表 → Rust 源码
// 用法: node gen-lunar.js > ../rust/src/lunar_data.rs  (在 app 目录运行)
const { Solar, Lunar } = require('lunar-javascript');

const EPOCH = Date.UTC(1900, 0, 1);
function daynum(y, m, d) { return Math.round((Date.UTC(y, m - 1, d) - EPOCH) / 86400000); }

// --- 自检：闰月符号与节气 API ---
{
  const l = Solar.fromYmd(2023, 3, 22).getLunar(); // 2023 闰二月初一
  console.error('check 2023-03-22:', l.getYear(), l.getMonth(), l.getDay());
  if (l.getMonth() !== -2 || l.getDay() !== 1) { console.error('LEAP MONTH SIGN UNEXPECTED'); process.exit(1); }
  const jq = Solar.fromYmd(2026, 9, 7).getLunar().getJieQi();
  console.error('check 2026-09-07 jieqi:', jq);
  if (jq !== '白露') { console.error('JIEQI API UNEXPECTED'); process.exit(1); }
}

const TERM_NAMES = ['小寒','大寒','立春','雨水','惊蛰','春分','清明','谷雨','立夏','小满','芒种','夏至','小暑','大暑','立秋','处暑','白露','秋分','寒露','霜降','立冬','小雪','大雪','冬至'];
const termIdx = {}; TERM_NAMES.forEach((n, i) => termIdx[n] = i);

// 单遍扫描
const years = new Map();   // ly -> { newYear, months: Map(m -> len), leap, leapLen }
const terms = [];          // terms[solarYear][idx] = day
let curKey = null, curLen = 0;

let d = Solar.fromYmd(1900, 1, 1);
const end = Solar.fromYmd(2101, 3, 10);
let guard = 0;
while (guard++ < 100000) {
  const y = d.getYear(), m = d.getMonth(), day = d.getDay();
  const l = d.getLunar();
  const ly = l.getYear(), lm = l.getMonth(), ld = l.getDay();

  const jq = l.getJieQi();
  if (jq) {
    const sy = y; // 节气按公历年归档
    if (!terms[sy]) terms[sy] = new Array(24).fill(0);
    terms[sy][termIdx[jq]] = day;
    const expectMonth = Math.floor(termIdx[jq] / 2) + 1;
    if (m !== expectMonth) { console.error('TERM MONTH MISMATCH', sy, jq, m); process.exit(1); }
  }

  const key = ly + ':' + lm;
  if (key !== curKey) {
    if (curKey !== null) {
      const [ply, plm] = curKey.split(':').map(Number);
      let rec = years.get(ply);
      if (!rec) { rec = { newYear: 0, months: new Map(), leap: 0, leapLen: 0 }; years.set(ply, rec); }
      if (plm > 0) rec.months.set(plm, curLen);
      else { rec.leap = -plm; rec.leapLen = curLen; }
    }
    curKey = key; curLen = 0;
    if (ld === 1 && lm === 1) {
      let rec = years.get(ly);
      if (!rec) { rec = { newYear: 0, months: new Map(), leap: 0, leapLen: 0 }; years.set(ly, rec); }
      rec.newYear = daynum(y, m, day);
    }
  }
  curLen++;

  if (y === end.getYear() && m === end.getMonth() && day === end.getDay()) break;
  d = d.nextDay(1);
}
console.error('scanned days:', guard);

// 输出 Rust
const Y0 = 1900, N = 201;
let lines = [];
lines.push('// 自动生成：node gen-lunar.js（数据源 lunar-javascript 6tail，覆盖 1900-2100）。请勿手改。');
lines.push('/// (new_year_offset: 距 1900-01-01 的天数, month_len_bits: bit0..11=1..12月且bit12=闰月是否30天, leap_month)');
lines.push('#[rustfmt::skip]');
lines.push('pub static LUNAR_YEARS: [(u32, u16, u8); ' + N + '] = [');
for (let y = Y0; y < Y0 + N; y++) {
  const rec = years.get(y);
  if (!rec || !rec.newYear) { console.error('MISSING YEAR', y); process.exit(1); }
  let bits = 0;
  for (let m = 1; m <= 12; m++) {
    const len = rec.months.get(m);
    if (len !== 29 && len !== 30) { console.error('BAD MONTH LEN', y, m, len); process.exit(1); }
    if (len === 30) bits |= 1 << (m - 1);
  }
  let leap = rec.leap || 0;
  if (leap) {
    if (leap < 1 || leap > 12) { console.error('BAD LEAP', y, leap); process.exit(1); }
    if (rec.leapLen === 30) bits |= 1 << 12;
  }
  lines.push('    (' + rec.newYear + ', ' + bits + ', ' + leap + '),');
}
lines.push('];');
lines.push('');
lines.push('/// [公历年-1900][节气0..23] = 当月几日；节气 t 的月份 = t/2+1');
lines.push('#[rustfmt::skip]');
lines.push('pub static SOLAR_TERM_DAYS: [[u8; 24]; ' + N + '] = [');
for (let y = Y0; y < Y0 + N; y++) {
  const row = terms[y];
  if (!row || row.some((v) => v < 1 || v > 31)) { console.error('MISSING TERMS', y, row); process.exit(1); }
  lines.push('    [' + row.join(', ') + '],');
}
lines.push('];');
lines.push('');
lines.push('pub static TERM_NAMES: [&str; 24] = [' + TERM_NAMES.map((n) => '"' + n + '"').join(', ') + '];');
console.log(lines.join('\n'));
