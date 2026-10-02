//! Event-driven capacitor simulator (behaviour-compatible with Pyfa eos/capSim.py, LGPL).
use std::cmp::Ordering;

#[derive(Debug, Clone, Copy)]
pub struct Drain {
    /// cycle duration in ms
    pub duration: f64,
    /// cap used per cycle (negative = cap injected)
    pub cap_need: f64,
    /// shots before reload (0 = infinite)
    pub clip_size: u32,
    pub reload_ms: f64,
    pub is_injector: bool,
    pub disable_stagger: bool,
}

#[derive(Debug, Clone)]
pub struct CapResult {
    pub stable: bool,
    /// lowest cap fraction reached while stable (0..1)
    pub stable_low: f64,
    pub stable_high: f64,
    /// time (s) at which the cap ran out (unstable) or simulation end
    pub t_s: f64,
    pub depletes_in_s: Option<f64>,
    pub eve_stable: f64,
    pub iterations: u64,
}

/// static part of an event source (never changes while simulating)
#[derive(Debug, Clone, Copy)]
struct Source {
    duration: f64,
    cap_need: f64,
    clip: u32,
    reload: f64,
    inj: bool,
}

/// heap entry: Pyfa's heapq orders `[t, duration, capNeed, shot, clipSize, reloadTime, isInjector]` then insertion
/// order; the static fields are replaced by their precomputed ranks (r1 = (duration, capNeed), r2 = (clip, reload,
/// inj)), which gives exactly the same order. The order is packed into three u64 keys compared lexicographically:
/// k0 = bits of t (t >= 0, so the IEEE bit pattern orders like the value), k1 = r1 << 32 | shot, k2 = r2 << 40 | seq.
#[derive(Debug, Clone, Copy)]
struct Ev {
    k0: u64,
    k1: u64,
    k2: u64,
    src: u32,
}
impl Ev {
    #[inline]
    fn new(t: f64, r1: u32, shot: u32, r2: u32, src: u32, seq: u64) -> Ev {
        debug_assert!(t >= 0.0 && seq < (1 << 40));
        Ev { k0: (t + 0.0).to_bits(), k1: (r1 as u64) << 32 | shot as u64, k2: (r2 as u64) << 40 | seq, src }
    }
    #[inline]
    fn t(&self) -> f64 {
        f64::from_bits(self.k0)
    }
    #[inline]
    fn shot(&self) -> u32 {
        self.k1 as u32
    }
    /// next activation: new time, shot and insertion sequence (rank fields unchanged)
    #[inline]
    fn reschedule(&mut self, t: f64, shot: u32, seq: u64) {
        self.k0 = (t + 0.0).to_bits();
        self.k1 = (self.k1 & !0xffff_ffff) | shot as u64;
        self.k2 = (self.k2 & !((1u64 << 40) - 1)) | seq;
    }
    #[inline]
    fn lt(&self, o: &Ev) -> bool {
        (self.k0, self.k1, self.k2) < (o.k0, o.k1, o.k2)
    }
}

/// minimal binary min-heap on `Ev::lt` (entries are small Copy values)
struct Heap {
    v: Vec<Ev>,
}
impl Heap {
    #[inline]
    fn peek(&self) -> Option<&Ev> {
        self.v.first()
    }
    fn push(&mut self, e: Ev) {
        let mut i = self.v.len();
        self.v.push(e);
        while i > 0 {
            let p = (i - 1) / 2;
            if e.lt(&self.v[p]) {
                self.v[i] = self.v[p];
                i = p;
            } else {
                break;
            }
        }
        self.v[i] = e;
    }
    #[inline]
    fn sift_down(&mut self, mut i: usize, e: Ev) {
        let n = self.v.len();
        loop {
            let l = 2 * i + 1;
            if l >= n {
                break;
            }
            let r = l + 1;
            let c = if r < n && self.v[r].lt(&self.v[l]) { r } else { l };
            if self.v[c].lt(&e) {
                self.v[i] = self.v[c];
                i = c;
            } else {
                break;
            }
        }
        self.v[i] = e;
    }
    /// replace the top entry and restore the heap
    #[inline]
    fn replace_top(&mut self, e: Ev) {
        self.sift_down(0, e);
    }
    fn pop(&mut self) -> Option<Ev> {
        let last = self.v.pop()?;
        if self.v.is_empty() {
            return Some(last);
        }
        let top = self.v[0];
        self.sift_down(0, last);
        Some(top)
    }
}

fn gcd(a: u64, b: u64) -> u64 {
    if b == 0 { a } else { gcd(b, a % b) }
}

#[allow(unused_assignments)] // take!() before a break
pub fn simulate(capacity: f64, recharge_ms: f64, drains: &[Drain], start_frac: f64, reload: bool, stagger: bool, t_max_ms: f64) -> CapResult {
    let tau = recharge_ms / 5.0;
    let mut heap = Heap { v: Vec::with_capacity(drains.len() + 4) };
    // (source, initial t) in insertion order; ranks are assigned once all sources are known
    let mut sources: Vec<Source> = Vec::new();
    let mut initial: Vec<(u32, f64)> = Vec::new();
    let mut seq = 0u64;
    let mut period: u64 = 1;
    let mut disable_period = false;
    // group identical modules
    let mut groups: Vec<(Drain, u32)> = Vec::new();
    for d in drains {
        let mut d = *d;
        if !reload && !d.is_injector {
            d.clip_size = 0;
            d.reload_ms = 0.0;
        }
        if d.duration <= 0.0 {
            continue;
        }
        if let Some(g) = groups.iter_mut().find(|(x, _)| {
            x.duration == d.duration && x.cap_need == d.cap_need && x.clip_size == d.clip_size && x.reload_ms == d.reload_ms
                && x.is_injector == d.is_injector && x.disable_stagger == d.disable_stagger
        }) {
            g.1 += 1;
        } else {
            groups.push((d, 1));
        }
    }
    for (d, n) in &groups {
        let mut d = *d;
        if d.clip_size > 0 {
            disable_period = true;
        }
        if d.is_injector {
            sources.push(Source { duration: d.duration, cap_need: d.cap_need, clip: d.clip_size, reload: d.reload_ms, inj: true });
            for _ in 0..*n {
                initial.push((sources.len() as u32 - 1, 0.0));
            }
            continue;
        }
        if stagger && !d.disable_stagger {
            if d.clip_size == 0 {
                d.duration = (d.duration / *n as f64).floor();
            } else {
                let st = (d.duration * d.clip_size as f64 + d.reload_ms) / (*n as f64 * d.clip_size as f64);
                sources.push(Source { duration: d.duration, cap_need: d.cap_need, clip: d.clip_size, reload: d.reload_ms, inj: false });
                for i in 1..*n {
                    initial.push((sources.len() as u32 - 1, i as f64 * st));
                }
            }
        } else {
            d.cap_need *= *n as f64;
        }
        let dur = d.duration.round().max(1.0) as u64;
        period = period / gcd(period, dur) * dur;
        sources.push(Source { duration: d.duration, cap_need: d.cap_need, clip: d.clip_size, reload: d.reload_ms, inj: false });
        initial.push((sources.len() as u32 - 1, 0.0));
    }
    // ranks of the static tie-break tuples (equal tuples share a rank)
    let rank = |cmp: &dyn Fn(&Source, &Source) -> Ordering| -> Vec<u32> {
        let mut idx: Vec<usize> = (0..sources.len()).collect();
        idx.sort_by(|&a, &b| cmp(&sources[a], &sources[b]));
        let mut r = vec![0u32; sources.len()];
        let mut cur = 0u32;
        for k in 0..idx.len() {
            if k > 0 && cmp(&sources[idx[k - 1]], &sources[idx[k]]) != Ordering::Equal {
                cur += 1;
            }
            r[idx[k]] = cur;
        }
        r
    };
    let fc = |a: f64, b: f64| a.partial_cmp(&b).unwrap_or(Ordering::Equal);
    let r1 = rank(&|a, b| fc(a.duration, b.duration).then_with(|| fc(a.cap_need, b.cap_need)));
    let r2 = rank(&|a, b| a.clip.cmp(&b.clip).then_with(|| fc(a.reload, b.reload)).then_with(|| a.inj.cmp(&b.inj)));
    for &(si, t) in &initial {
        heap.push(Ev::new(t, r1[si as usize], 0, r2[si as usize], si, seq));
        seq += 1;
    }
    let period = if disable_period || period as f64 > t_max_ms { t_max_ms } else { period as f64 };

    let cap_max = capacity;
    let mut cap = capacity * start_frac;
    let mut cap_wrap = cap;
    let mut cap_lowest = cap;
    let mut cap_lowest_pre = cap;
    let mut t_wrap = period;
    let mut t_last = 0.0f64;
    let mut iterations = 0u64;
    let mut awaiting: Vec<Ev> = Vec::new();
    let mut awaiting_wrap: Vec<(u64, u64)> = Vec::new();
    let mut ran_out = false;
    let key = |v: &Vec<Ev>| {
        let mut k: Vec<(u64, u64)> = v.iter().map(|e| (sources[e.src as usize].duration.to_bits(), sources[e.src as usize].cap_need.to_bits())).collect();
        k.sort();
        k
    };
    let mut last_ev: Option<Ev> = None;
    let mut exp_cache = [(0u64, 0.0f64, false); 64];
    // The pop order of a priority queue depends only on the set of entries (the order is total: seq is unique), so
    // the current event stays in the heap and is updated in place (one sift-down) unless something else must be
    // pushed first or it leaves the simulation.
    while let Some(&top) = heap.peek() {
        let mut ev = top;
        let mut in_heap = true;
        macro_rules! take {
            () => {
                if in_heap {
                    heap.pop();
                    in_heap = false;
                }
            };
        }
        let sv = sources[ev.src as usize];
        let t_now = ev.t();
        if t_now >= t_max_ms {
            take!();
            last_ev = Some(ev);
            break;
        }
        if t_now > t_last && cap_max > 0.0 && tau > 0.0 {
            let x = (cap / cap_max).max(0.0).sqrt();
            // exp of a repeated argument (event times are periodic): exact memo on the argument's bits
            let arg = (t_last - t_now) / tau;
            let slot = &mut exp_cache[(arg.to_bits() as usize ^ (arg.to_bits() >> 29) as usize) & 63];
            let e = if slot.0 == arg.to_bits() && slot.2 { slot.1 } else {
                let e = arg.exp();
                *slot = (arg.to_bits(), e, true);
                e
            };
            cap = (1.0 + (x - 1.0) * e).powi(2) * cap_max;
        }
        if t_now != t_last {
            if cap < cap_lowest_pre {
                cap_lowest_pre = cap;
            }
            if t_now == t_wrap {
                let k = key(&awaiting);
                if cap >= cap_wrap && k == awaiting_wrap {
                    take!();
                    last_ev = Some(ev);
                    break;
                }
                cap_wrap = (cap * 10.0).round() / 10.0;
                awaiting_wrap = k;
                t_wrap += period;
            }
        }
        t_last = t_now;
        iterations += 1;
        if iterations > 5_000_000 {
            take!();
            last_ev = Some(ev);
            break;
        }
        if sv.inj && cap - sv.cap_need > cap_max {
            take!();
            awaiting.push(ev);
            continue;
        }
        let cn = |e: &Ev| sources[e.src as usize].cap_need;
        if sv.cap_need > cap && cap < cap_max {
            while !awaiting.is_empty() && sv.cap_need > cap && cap_max > cap {
                let need = (sv.cap_need - cap).min(cap_max - cap);
                let good: Vec<usize> = (0..awaiting.len()).filter(|&i| -cn(&awaiting[i]) >= need).collect();
                let pick = if !good.is_empty() {
                    *good.iter().min_by(|&&a, &&b| (-cn(&awaiting[a])).partial_cmp(&-cn(&awaiting[b])).unwrap()).unwrap()
                } else {
                    (0..awaiting.len()).max_by(|&a, &b| (-cn(&awaiting[a])).partial_cmp(&-cn(&awaiting[b])).unwrap()).unwrap()
                };
                take!();
                let mut inj = awaiting.remove(pick);
                let is = sources[inj.src as usize];
                cap = (cap - is.cap_need).min(cap_max);
                let (mut nt, mut shot) = (t_now + is.duration, inj.shot() + 1);
                if is.clip > 0 && shot % is.clip == 0 {
                    shot = 0;
                    nt += is.reload;
                }
                inj.reschedule(nt, shot, seq);
                seq += 1;
                heap.push(inj);
            }
        }
        cap = (cap - sv.cap_need).min(cap_max);
        if cap < cap_lowest {
            if cap < 0.0 {
                take!();
                ran_out = true;
                last_ev = Some(ev);
                break;
            }
            cap_lowest = cap;
        }
        while !awaiting.is_empty() && cap < cap_max {
            let need = cap_max - cap;
            let good: Vec<usize> = (0..awaiting.len()).filter(|&i| -cn(&awaiting[i]) <= need).collect();
            if good.is_empty() {
                break;
            }
            let pick = *good.iter().max_by(|&&a, &&b| (-cn(&awaiting[a])).partial_cmp(&-cn(&awaiting[b])).unwrap()).unwrap();
            take!();
            let mut inj = awaiting.remove(pick);
            let is = sources[inj.src as usize];
            cap = (cap - is.cap_need).min(cap_max);
            let (mut nt, mut shot) = (t_now + is.duration, inj.shot() + 1);
            if is.clip > 0 && shot % is.clip == 0 {
                shot = 0;
                nt += is.reload;
            }
            inj.reschedule(nt, shot, seq);
            seq += 1;
            heap.push(inj);
        }
        let (mut nt, mut shot) = (t_now + sv.duration, ev.shot() + 1);
        if sv.clip > 0 && shot % sv.clip == 0 {
            shot = 0;
            nt += sv.reload;
        }
        ev.reschedule(nt, shot, seq);
        seq += 1;
        if in_heap {
            heap.replace_top(ev);
        } else {
            heap.push(ev);
        }
    }
    // EVE's own stability estimate
    let mut all: Vec<Ev> = heap.v;
    if let Some(e) = last_ev {
        all.push(e);
    }
    let avg_drain: f64 = all.iter().map(|e| sources[e.src as usize].cap_need / sources[e.src as usize].duration).sum();
    let inner = -(2.0 * avg_drain * tau - cap_max) / cap_max;
    let eve_stable = if inner >= 0.0 && cap_max > 0.0 { 0.25 * (1.0 + inner.sqrt()).powi(2) } else { 0.0 };
    let stable = !ran_out;
    CapResult {
        stable,
        stable_low: if stable && cap_max > 0.0 { cap_lowest / cap_max } else { 0.0 },
        stable_high: if stable && cap_max > 0.0 { cap_lowest_pre / cap_max } else { 0.0 },
        t_s: t_last / 1000.0,
        depletes_in_s: if stable { None } else { Some(t_last / 1000.0) },
        eve_stable,
        iterations,
    }
}
