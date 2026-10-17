/**
 * The haunted props: a skeleton key, a cratered moon, a jack-o'-lantern, a chamber candle, a
 * bat, a FANG coin, a coffin, a gravestone, a cauldron and a black cat. Each is a Scene for
 * the ASCII renderer in ./engine. Object space: x right, y up, z toward the viewer, sized to
 * fit within about one unit of the origin.
 */
import {
  BLOOD,
  clamp,
  type Frame,
  GLOW,
  glyph,
  GOLD,
  type Hit,
  INK,
  march,
  mul,
  noise3,
  rotX,
  rotY,
  rotZ,
  type Scene,
  sdBox,
  sdCapsule,
  sdCylX,
  sdCylY,
  sdEllipsoid,
  sdRoundCone,
  sdTorusY,
  sdTorusZ,
  smin,
  SURFACE,
} from "./engine";

export type PropKind = "key" | "moon" | "pumpkin" | "candle" | "bat" | "coin" | "coffin" | "grave" | "cauldron" | "cat";

export function makeScene(kind: PropKind): Scene {
  switch (kind) {
    case "key":
      return keyScene();
    case "moon":
      return moonScene();
    case "pumpkin":
      return pumpkinScene();
    case "candle":
      return candleScene();
    case "bat":
      return batScene();
    case "coin":
      return coinScene();
    case "coffin":
      return coffinScene();
    case "grave":
      return graveScene();
    case "cauldron":
      return cauldronScene();
    case "cat":
      return catScene();
  }
}

// Lettering on a face-on plane. Rays pass through the screen's cell centres, so letters are
// placed on that lattice (offset by half a cell when the grid or the text has even length):
// face-on, every letter lands in exactly one cell. The face sits a little in front of the
// origin, so it projects a touch larger than a cell there.
const lattice = (f: Frame) => ({
  cw: (f.cellW ?? 0.05) * 0.985,
  ch: (f.cellH ?? 0.1) * 0.985,
  colRay: (f.cols ?? 1) % 2 ? 0 : 0.5,
  rowRay: (f.rows ?? 1) % 2 ? 0 : 0.5,
});

/** Column and row (counting up) of the cell a point on a face-on plane falls in. */
function cellAt(x: number, y: number, f: Frame): [number, number] {
  const { cw, ch, colRay, rowRay } = lattice(f);
  return [Math.round(x / cw - colRay), Math.round(y / ch - rowRay)];
}

/** The glyph of centred `lines` of text at (x, y); the top line is centred near y = top. */
function lettering(lines: string[], x: number, y: number, top: number, f: Frame) {
  const { cw, ch, colRay, rowRay } = lattice(f);
  const vTop = Math.round(top / ch - rowRay) + rowRay;
  const line = Math.round(vTop - y / ch);
  if (line < 0 || line >= lines.length) return 0;
  const text = lines[line], n = text.length;
  const i = Math.round(x / cw - colRay - (n % 2 ? 0 : 0.5) + (n - 1) / 2);
  return i >= 0 && i < n ? glyph(text[i]) : 0;
}

/** Distance to a few puffs of smoke rising from (x0, y0, z0); they drift and swell as they go. */
function smoke(x: number, y: number, z: number, x0: number, y0: number, z0: number, t: number, n: number, rise: number, rate: number) {
  let d = 1e9;
  for (let i = 0; i < n; i++) {
    const k = (t * rate + i / n) % 1;
    const px = x0 + Math.sin(t * 0.9 + i * 2.1) * 0.12 * k, py = y0 + k * rise;
    d = Math.min(d, Math.hypot(x - px, y - py, z - z0) - (0.025 + 0.05 * k) * (1 - 0.7 * k * k));
  }
  return d;
}
const G_WISP = glyph("~"), G_TICK = glyph("'");

// ---------------------------------------------------------------------------------------------
// Skeleton key: ring bow, collars, a long shaft and a three-ward bit. It turns as if in a lock.

function keyScene(): Scene {
  const sdf = (x: number, y: number, z: number) => {
    let d = sdTorusZ(x + 0.8, y, z, 0.225, 0.07);
    d = Math.min(d, sdCylX(x + 0.47, y, z, 0.12, 0.03));
    d = Math.min(d, sdCylX(x + 0.385, y, z, 0.092, 0.022));
    d = Math.min(d, sdCylX(x - 0.285, y, z, 0.056, 0.815));
    d = Math.min(d, sdBox(x - 0.715, y + 0.155, z, 0.065, 0.105, 0.032));
    d = Math.min(d, sdBox(x - 0.92, y + 0.19, z, 0.062, 0.14, 0.032));
    d = Math.min(d, sdBox(x - 1.06, y + 0.125, z, 0.04, 0.075, 0.032));
    return d;
  };
  return {
    radius: 1.2,
    fit: [1.2, 0.4],
    pose: (f) => mul(rotY(0.28 * Math.sin(f.t * 0.5) + f.lookX * 0.45), rotZ(0.12 * f.lookY), rotX(f.t * 0.7 + f.spin)),
    intersect(ox, oy, oz, dx, dy, dz, t0, t1, h) {
      if (!march(sdf, ox, oy, oz, dx, dy, dz, t0, t1, h)) return false;
      h.mat = SURFACE;
      h.value = 0.95;
      return true;
    },
  };
}

// ---------------------------------------------------------------------------------------------
// Moon: a sphere with dark maria and craters (normal-mapped bowls with raised rims).

function moonScene(): Scene {
  const craters: number[][] = [];
  let s = 7;
  const rnd = () => ((s = (s * 16807) % 2147483647) / 2147483647);
  for (let i = 0; i < 22; i++) {
    const u = rnd() * 2 - 1, a = rnd() * Math.PI * 2, q = Math.sqrt(1 - u * u);
    craters.push([q * Math.cos(a), u, q * Math.sin(a), 0.07 + Math.pow(rnd(), 2) * 0.24]);
  }
  return {
    radius: 1,
    fit: [1.02, 1.02],
    maxInk: 0.93,
    pose: (f) => mul(rotZ(0.35), rotX(0.25 + f.lookY * 0.15), rotY(f.t * 0.06 + f.spin + f.lookX * 0.3)),
    intersect(ox, oy, oz, dx, dy, dz, t0, _t1, h) {
      const x = ox + dx * t0, y = oy + dy * t0, z = oz + dz * t0;
      let nx = x, ny = y, nz = z;
      // Maria: big soft dark seas.
      const sea = noise3(x * 1.6 + 5, y * 1.6, z * 1.6) * 0.65 + noise3(x * 3.4, y * 3.4 + 2, z * 3.4) * 0.35;
      let albedo = 1 - 0.5 * clamp((sea - 0.45) * 3.2);
      for (const [cx, cy, cz, r] of craters) {
        const cos = x * cx + y * cy + z * cz;
        const dist = Math.sqrt(Math.max(0, 2 - 2 * cos));
        const u = dist / r;
        if (u >= 1.45) continue;
        // Slope of the crater profile: bowl inside, falling rim outside.
        const slope = u < 1 ? 2 * u : -2.2 * (1.45 - u);
        let tx = x - cx * cos, ty = y - cy * cos, tz = z - cz * cos;
        const tl = Math.hypot(tx, ty, tz) || 1;
        tx /= tl;
        ty /= tl;
        tz /= tl;
        nx -= 0.32 * slope * tx;
        ny -= 0.32 * slope * ty;
        nz -= 0.32 * slope * tz;
        if (u < 0.85) albedo *= 0.86;
      }
      const l = Math.hypot(nx, ny, nz);
      h.t = t0;
      h.x = x;
      h.y = y;
      h.z = z;
      h.nx = nx / l;
      h.ny = ny / l;
      h.nz = nz / l;
      h.mat = SURFACE;
      h.value = albedo;
      return true;
    },
    light(h, lx, ly, lz) {
      // The terminator stays crisp; a little earthshine keeps the dark limb from going solid.
      const d = h.nx * lx + h.ny * ly + h.nz * lz;
      return clamp(h.value * Math.pow(Math.max(0, d), 0.8) * 1.08 + 0.05);
    },
  };
}

// ---------------------------------------------------------------------------------------------
// Jack-o'-lantern: a ribbed, hollow pumpkin with a carved face lit by a guttering candle.

/** Signed distance-ish to a triangle (either winding): negative inside, never overestimates outside. */
const tri = (x: number, y: number, ax: number, ay: number, bx: number, by: number, cx: number, cy: number) => {
  const o = Math.sign((bx - ax) * (cy - ay) - (by - ay) * (cx - ax));
  const e = (px: number, py: number, qx: number, qy: number) => {
    const nx = qy - py, ny = px - qx, l = Math.hypot(nx, ny);
    return (o * ((x - px) * nx + (y - py) * ny)) / l;
  };
  return Math.max(e(ax, ay, bx, by), e(bx, by, cx, cy), e(cx, cy, ax, ay));
};
const box2 = (x: number, y: number, cx: number, cy: number, hx: number, hy: number) => Math.max(Math.abs(x - cx) - hx, Math.abs(y - cy) - hy);

/** The carved face; `blink` (0..1) squeezes the eyes shut. */
function face(x: number, y: number, blink: number) {
  const ax = Math.abs(x);
  // Eyes (mirrored) and nose: triangles.
  const ey = 0.2 + (y - 0.2) / Math.max(0.1, 1 - blink);
  let d = tri(ax, ey, 0.13, 0.1, 0.33, 0.4, 0.52, 0.1) * Math.max(0.1, 1 - blink);
  d = Math.min(d, tri(x, y, -0.09, -0.09, 0, 0.06, 0.09, -0.09));
  // A wide grin with two teeth.
  let m = Math.max(y - (-0.19 + 0.5 * x * x), -0.43 + 0.78 * x * x - y, ax - 0.6);
  m = Math.max(m, -box2(x, y, -0.17, -0.2, 0.065, 0.085), -box2(x, y, 0.17, -0.39, 0.065, 0.085));
  return Math.min(d, m * 0.8);
}

function pumpkinScene(): Scene {
  let flicker = 1, blink = 0;
  const body = (x: number, y: number, z: number) => {
    const yy = y * 1.3;
    const r = Math.hypot(x, yy, z);
    const rxz = Math.hypot(x, z);
    const phi = Math.atan2(z, x);
    const g = 0.5 + 0.5 * Math.cos(8 * phi + Math.PI);
    const groove = g * g * g * g * g * g;
    const w = rxz / (r + 1e-6);
    const dimple = 0.16 * Math.exp(-rxz * rxz * 9) * (y > 0 ? 1 : 0.5);
    return (r - (1 - 0.085 * groove * w - dimple)) * 0.7;
  };
  const sdf = (x: number, y: number, z: number) => {
    const b = body(x, y, z);
    const shell = Math.max(b, -(b + 0.07));
    // Well clear of the face (or behind it) the carving can't matter, so skip it.
    const nearFace = z > -0.1 && Math.abs(x) < 0.72 && y < 0.52 && y > -0.6;
    const carved = nearFace ? Math.max(shell, -Math.max(face(x, y, blink), -z)) : shell;
    // The stem: a stubby, bent, tapering capsule.
    const bend = (y - 0.55) * (y - 0.55) * 1.4;
    const stem = sdCapsule(x - bend, y, z, 0, 0.5, 0, 0.02, 0.86, 0, 0.12 - Math.max(0, y - 0.55) * 0.12);
    return smin(carved, stem, 0.04);
  };
  return {
    radius: 1.08,
    fit: [1.05, 1.02],
    pose(f) {
      // Startled (the cursor rushed at it): the candle gutters nearly out, then flares back.
      const fright = f.fright ?? 0;
      flicker = (0.78 + 0.22 * noise3(f.t * 6, 0.5, 0.5) + 0.08 * Math.sin(f.t * 23)) * (1 - 0.9 * fright * fright) + 0.25 * fright * (1 - fright);
      // ...and screws its eyes shut. Otherwise it blinks every seven seconds or so.
      const lid = f.t % 7.3;
      blink = Math.max(fright > 0.35 ? 1 : fright / 0.35, lid < 0.16 ? Math.sin((lid / 0.16) * Math.PI) : 0);
      return mul(rotY(0.35 * Math.sin(f.t * 0.45) + f.lookX * 0.75 + f.spin), rotX(0.18 - f.lookY * 0.35));
    },
    intersect(ox, oy, oz, dx, dy, dz, t0, t1, h) {
      if (!march(sdf, ox, oy, oz, dx, dy, dz, t0, t1, h, 0.85, 110)) return false;
      const b = body(h.x, h.y, h.z);
      if (b < -0.02) {
        // Inside the shell: lit by the candle sitting low in the middle.
        const cx = h.x, cy = h.y + 0.25, cz = h.z;
        const dist2 = cx * cx + cy * cy + cz * cz;
        h.mat = GLOW;
        h.value = clamp((0.55 / (0.35 + dist2)) * flicker * (b < -0.05 ? 1 : 0.7));
      } else {
        h.mat = SURFACE;
        h.value = h.y > 0.62 ? 0.55 : 0.95;
      }
      return true;
    },
  };
}

// ---------------------------------------------------------------------------------------------
// Chamber candle: dripping wax on a pool of spilt wax, in a socketed dish with a finger loop and
// thumb rest. A charred wick, a swaying flame (the light), and a thread of smoke.

function candleScene(): Scene {
  let sway = 0, tall = 1, flicker = 1, time = 0;
  const FY = 0.56;
  const DRIPS = [
    [0.4, 0.32],
    [1.3, 0.12],
    [1.9, 0.2],
    [2.9, 0.48],
    [3.7, 0.08],
    [4.3, 0.27],
    [5.4, 0.14],
  ];
  const wax = (x: number, y: number, z: number) => {
    let d = sdCylY(x, y + 0.21, z, 0.21, 0.6);
    // A lopsided, melted crown: cupped in the middle, lumpy round the rim.
    const a = Math.atan2(z, x);
    d = Math.max(d, y - 0.39 - 0.035 * Math.sin(a * 3 + 1) - 0.02 * Math.sin(a * 7));
    d = Math.max(d, -(Math.hypot(x, y - 0.6, z) - 0.23));
    for (const [ang, len] of DRIPS) {
      const px = Math.cos(ang) * 0.205, pz = Math.sin(ang) * 0.205;
      d = smin(d, sdCapsule(x, y, z, px, 0.36, pz, px, 0.36 - len, pz, 0.032 + len * 0.02), 0.03);
    }
    // Spilt wax pooled round the foot.
    return smin(d, sdEllipsoid(x - 0.04, y + 0.76, z + 0.02, 0.36, 0.035, 0.33), 0.06);
  };
  const dish = (x: number, y: number, z: number) =>
    Math.min(
      sdCylY(x, y + 0.84, z, 0.56, 0.025),
      sdTorusY(x, y + 0.81, z, 0.55, 0.035),
      sdTorusY(x, y + 0.74, z, 0.245, 0.035), // the socket
      sdTorusZ(x - 0.67, y + 0.79, z, 0.11, 0.028), // finger loop
      sdCylY(x - 0.69, y + 0.66, z, 0.08, 0.012), // thumb rest
    );
  const flame = (x: number, y: number, z: number) => {
    const yy = (y - FY) / tall;
    const xx = x - sway * Math.max(0, yy + 0.1);
    // Teardrop: an ellipsoid whose top narrows to a point.
    const pinch = 1 + Math.max(0, yy) * 5.5;
    return (Math.hypot(xx * pinch, yy * 0.62, z * pinch) - 0.085) * 0.6;
  };
  const wick = (x: number, y: number, z: number) => sdCapsule(x, y, z, 0, 0.4, 0, sway * 0.15, 0.5, 0, 0.014);
  const fumes = (x: number, y: number, z: number) => smoke(x, y, z, sway * 0.4, FY + 0.24, 0, time, 3, 0.3, 0.32);
  const sdf = (x: number, y: number, z: number) => Math.min(wax(x, y, z), dish(x, y, z), flame(x, y, z), wick(x, y, z), fumes(x, y, z));
  return {
    radius: 1.15,
    fit: [0.82, 1.06],
    pose(f) {
      time = f.t;
      flicker = 0.85 + 0.15 * noise3(f.t * 5, 1.5, 0);
      sway = (noise3(f.t * 1.7, 3, 0) - 0.5) * 0.5 - f.lookX * 0.15;
      tall = 0.9 + 0.25 * noise3(f.t * 3.1, 7, 0);
      return mul(rotX(0.32 - f.lookY * 0.2), rotY(f.t * 0.25 + f.lookX * 0.5 + f.spin));
    },
    intersect(ox, oy, oz, dx, dy, dz, t0, t1, h) {
      if (!march(sdf, ox, oy, oz, dx, dy, dz, t0, t1, h)) return false;
      h.mat = SURFACE;
      if (flame(h.x, h.y, h.z) < 0.004) {
        h.mat = GLOW;
        // Hotter (ember) at the root of the flame.
        h.value = clamp((1.05 - Math.max(0, h.y - FY) * 2.2) * flicker);
      } else if (fumes(h.x, h.y, h.z) < 0.004) {
        h.value = 0.8;
        h.glyph = h.y > FY + 0.4 ? G_WISP : G_TICK;
      } else if (wick(h.x, h.y, h.z) < 0.004) {
        h.value = 0.12;
      } else {
        h.value = h.y < -0.7 && Math.hypot(h.x, h.z) > 0.3 ? 0.7 : 0.98;
      }
      return true;
    },
    light(h, lx, ly, lz) {
      // Point light at the flame, plus a weak fill from the cursor lantern.
      const fx = -h.x, fy = FY - h.y, fz = -h.z;
      const fl = Math.hypot(fx, fy, fz) || 1;
      // Wrapped so the flame's light reaches round the sides of the wax.
      const lit = Math.max(0, ((h.nx * fx + h.ny * fy + h.nz * fz) / fl + 0.6) / 1.6) * (1.1 / (0.5 + fl * fl)) * flicker;
      const fill = Math.max(0, h.nx * lx + h.ny * ly + h.nz * lz) * 0.4;
      return clamp(h.value * (lit + fill + 0.06));
    },
  };
}

// ---------------------------------------------------------------------------------------------
// Bat: membrane wings (flat polygons hinged at the body) flapping around a furry body.

// Right wing outline, traced from the bat ornament (components/Ornament.tsx), in object units.
const WING = [
  [64, 22],
  [72, 21],
  [80, 16.5],
  [90, 9.5],
  [103, 6],
  [118, 5.5],
  [113.5, 10.5],
  [110.5, 16],
  [109.5, 24],
  [104.5, 21],
  [99.5, 22],
  [96, 28.5],
  [91, 25.5],
  [85, 27],
  [81.5, 34],
  [76.5, 32],
  [71.5, 35],
  [68.5, 42.5],
  [65.5, 41.5],
  [64, 41],
].map(([x, y]) => [(x - 60) / 55, (26 - y) / 55]);
const HINGE = WING[0][0];
// The arm runs along the leading edge to the wrist; finger bones fan out to the scallop points.
const WRIST = [(88 - 60) / 55, (26 - 11) / 55];
const BONES = [WING[0], ...[5, 8, 11, 14].map((i) => WING[i])];
// One ear (mirrored for the other), standing in the plane z = 0.02.
const EAR = [0.015, 0.17, 0.055, 0.31, 0.085, 0.15];

function inWing(x: number, y: number) {
  if (x < HINGE - 0.001 || x > 1.06 || y > 0.4 || y < -0.32) return false;
  let inside = false;
  for (let i = 0, j = WING.length - 1; i < WING.length; j = i++) {
    const [xi, yi] = WING[i], [xj, yj] = WING[j];
    if (yi > y !== yj > y && x < ((xj - xi) * (y - yi)) / (yj - yi) + xi) inside = !inside;
  }
  return inside;
}

function nearBone(x: number, y: number) {
  const px = x - WRIST[0], py = y - WRIST[1];
  for (const [fx, fy] of BONES) {
    const qx = fx - WRIST[0], qy = fy - WRIST[1];
    const k = clamp((px * qx + py * qy) / (qx * qx + qy * qy));
    if (Math.hypot(px - qx * k, py - qy * k) < 0.024) return true;
  }
  return false;
}

function batScene(): Scene {
  let flap = 0, bob = 0;
  // Ray vs. an ellipsoid at (cx, cy, cz) with radii (a, b, c).
  const ellipsoid = (ox: number, oy: number, oz: number, dx: number, dy: number, dz: number, cx: number, cy: number, cz: number, a: number, b: number, c: number, h: Hit) => {
    const px = (ox - cx) / a, py = (oy - cy) / b, pz = (oz - cz) / c, qx = dx / a, qy = dy / b, qz = dz / c;
    const A = qx * qx + qy * qy + qz * qz, B = px * qx + py * qy + pz * qz, C = px * px + py * py + pz * pz - 1;
    const disc = B * B - A * C;
    if (disc < 0) return false;
    const t = (-B - Math.sqrt(disc)) / A;
    if (t <= 0 || t >= h.t) return false;
    h.t = t;
    h.x = ox + dx * t;
    h.y = oy + dy * t;
    h.z = oz + dz * t;
    const nx = (h.x - cx) / (a * a), ny = (h.y - cy) / (b * b), nz = (h.z - cz) / (c * c);
    const l = Math.hypot(nx, ny, nz);
    h.nx = nx / l;
    h.ny = ny / l;
    h.nz = nz / l;
    h.value = 0.4;
    return true;
  };
  // Ray vs. the right wing, hinged at x = HINGE and rotated about the body axis (y) by `flap`.
  // `side` mirrors it to make the left wing.
  const wing = (ox: number, oy: number, oz: number, dx: number, dy: number, dz: number, side: number, h: Hit) => {
    const c = Math.cos(flap), s = Math.sin(flap);
    // Into wing space: mirror, translate to the hinge, rotate by -flap about y.
    const px = ox * side - HINGE, qx = dx * side;
    const lx = c * px - s * oz, lz = s * px + c * oz;
    const ux = c * qx - s * dz, uz = s * qx + c * dz;
    if (Math.abs(uz) < 1e-6) return false;
    const t = -lz / uz;
    if (t <= 0 || t >= h.t) return false;
    const wx = lx + ux * t + HINGE, wy = oy + dy * t;
    if (!inWing(wx, wy)) return false;
    h.t = t;
    h.x = ox + dx * t;
    h.y = wy;
    h.z = oz + dz * t;
    // Wing normal (0,0,1) rotated back by +flap, mirrored; face it toward the viewer.
    let nx = s * side, nz = c;
    if (nx * dx + nz * dz > 0) {
      nx = -nx;
      nz = -nz;
    }
    h.nx = nx;
    h.ny = 0;
    h.nz = nz;
    h.value = nearBone(wx, wy) ? 0.25 : 0.72;
    return true;
  };
  const ears = (ox: number, oy: number, oz: number, dx: number, dy: number, dz: number, h: Hit) => {
    if (Math.abs(dz) < 1e-6) return false;
    const t = (0.02 - oz) / dz;
    if (t <= 0 || t >= h.t) return false;
    const x = ox + dx * t, y = oy + dy * t;
    if (tri(Math.abs(x), y, EAR[0], EAR[1], EAR[2], EAR[3], EAR[4], EAR[5]) > 0) return false;
    h.t = t;
    h.x = x;
    h.y = y;
    h.z = 0.02;
    h.nx = 0;
    h.ny = 0;
    h.nz = dz < 0 ? 1 : -1;
    h.value = 0.35;
    return true;
  };
  return {
    radius: 1.12,
    fit: [1.1, 0.5],
    pose(f) {
      const ph = f.t * 6.5 + f.spin * 3;
      // Negative flap lifts the wingtips toward the viewer, which reads as "up" from below.
      flap = -0.1 + 0.62 * Math.sin(ph);
      bob = -0.05 * Math.cos(ph);
      return mul(rotY(0.4 * Math.sin(f.t * 0.4) + f.lookX * 0.5), rotX(-0.45 - f.lookY * 0.3));
    },
    intersect(ox, oy, oz, dx, dy, dz, _t0, t1, h) {
      oy -= bob;
      h.t = t1;
      let hit = ellipsoid(ox, oy, oz, dx, dy, dz, 0, -0.07, 0, 0.085, 0.26, 0.085, h);
      hit = ellipsoid(ox, oy, oz, dx, dy, dz, 0, 0.13, 0.015, 0.075, 0.07, 0.07, h) || hit;
      hit = wing(ox, oy, oz, dx, dy, dz, 1, h) || hit;
      hit = wing(ox, oy, oz, dx, dy, dz, -1, h) || hit;
      hit = ears(ox, oy, oz, dx, dy, dz, h) || hit;
      if (hit) h.mat = SURFACE;
      return hit;
    },
  };
}


// ---------------------------------------------------------------------------------------------
// FANG coin: a milled disc with a raised rim, its faces struck with F A N G in gold.

function coinScene(): Scene {
  let frame: Frame = { t: 0, lookX: 0, lookY: 0, spin: 0 };
  const R = 0.92, T = 0.085;
  const sdf = (x: number, y: number, z: number) => Math.min(sdCylX(z, x, y, R, T), sdTorusZ(x, y, z, R - 0.08, 0.105));
  const G_REED = glyph("|"), FANG = "FANG".split("").map(glyph);
  return {
    radius: 1.02,
    fit: [1, 1],
    pose(f) {
      frame = f;
      return mul(rotX(0.18 + f.lookY * 0.2), rotY(f.t * 0.9 + f.spin + f.lookX * 0.3));
    },
    intersect(ox, oy, oz, dx, dy, dz, t0, t1, h) {
      if (!march(sdf, ox, oy, oz, dx, dy, dz, t0, t1, h)) return false;
      h.mat = SURFACE;
      h.value = 0.9;
      const r = Math.hypot(h.x, h.y);
      if (Math.abs(h.z) > T - 0.01 && r < R - 0.2) {
        // Minted rows of FANG, staggered like brickwork; mirrored on the back so it reads.
        const [c, row] = cellAt(h.z > 0 ? h.x : -h.x, h.y, frame);
        h.glyph = FANG[(((c + row * 2) % 4) + 4) % 4];
        h.tint = GOLD;
      } else if (r > R - 0.005 && Math.abs(h.z) < T - 0.01) {
        h.glyph = G_REED; // the milled edge
      }
      return true;
    },
  };
}

// ---------------------------------------------------------------------------------------------
// Coffin: a six-sided box whose lid creaks open on its hinge. Something inside looks out.

function coffinScene(): Scene {
  let lid = 0, eyes = 0;
  // Outline seen from above (x along the coffin, z across it): narrow head, wide shoulders.
  const OUTLINE = [
    [-1, -0.22],
    [-0.55, -0.38],
    [1, -0.2],
    [1, 0.2],
    [-0.55, 0.38],
    [-1, 0.22],
  ];
  const outline = (x: number, z: number) => {
    let d = -1e9;
    for (let i = 0; i < OUTLINE.length; i++) {
      const [ax, az] = OUTLINE[i], [bx, bz] = OUTLINE[(i + 1) % OUTLINE.length];
      const nx = bz - az, nz = ax - bx, l = Math.hypot(nx, nz);
      d = Math.max(d, ((x - ax) * nx + (z - az) * nz) / l);
    }
    return d;
  };
  const box = (x: number, y: number, z: number) => {
    const p = outline(x, z);
    const outer = Math.max(p, Math.abs(y + 0.09) - 0.17);
    return Math.max(outer, -Math.max(p + 0.05, -(y + 0.2)));
  };
  // The lid, hinged along the far side (y = 0.08, z = -0.3), with a cross in relief.
  const cover = (x: number, y: number, z: number) => {
    const c = Math.cos(lid), s = Math.sin(lid), qy = y - 0.08, qz = z + 0.3;
    const ly = qy * c - qz * s + 0.08, lz = qy * s + qz * c - 0.3;
    const slab = Math.max(outline(x, lz), Math.abs(ly - 0.12) - 0.04);
    const cross = Math.min(sdBox(x + 0.1, ly - 0.17, lz, 0.5, 0.02, 0.035), sdBox(x + 0.32, ly - 0.17, lz, 0.035, 0.02, 0.17));
    return Math.min(slab, cross);
  };
  const peepers = (x: number, y: number, z: number) => Math.hypot(x + 0.66, y + 0.1, Math.abs(z) - 0.09) - 0.045;
  const sdf = (x: number, y: number, z: number) => Math.min(box(x, y, z), cover(x, y, z), eyes > 0 ? peepers(x, y, z) : 1e9);
  return {
    radius: 1.25,
    fit: [1.12, 0.72],
    maxInk: 0.85,
    pose(f) {
      // Creak open, hold, slam shut, wait.
      const p = (f.t % 4.6) / 4.6;
      const ease = (u: number) => u * u * (3 - 2 * u);
      lid = 0.95 * (p < 0.35 ? ease(p / 0.35) : p < 0.72 ? 1 : p < 0.78 ? 1 - ease((p - 0.72) / 0.06) : 0);
      eyes = lid > 0.25 && (f.t * 1.3) % 3.1 > 0.15 ? 1 : 0;
      return mul(rotX(0.62 - f.lookY * 0.2), rotY(-0.32 + 0.15 * Math.sin(f.t * 0.5) + f.lookX * 0.4));
    },
    intersect(ox, oy, oz, dx, dy, dz, t0, t1, h) {
      if (!march(sdf, ox, oy, oz, dx, dy, dz, t0, t1, h)) return false;
      if (eyes && peepers(h.x, h.y, h.z) < 0.004) {
        h.mat = GLOW;
        h.value = 0.95;
      } else {
        h.mat = SURFACE;
        // Inside the box is dark.
        h.value = outline(h.x, h.z) < -0.05 && h.y < 0.07 && box(h.x, h.y, h.z) < 0.004 ? 0.25 : 0.9;
      }
      return true;
    },
  };
}

// ---------------------------------------------------------------------------------------------
// Gravestone: an arched, chipped slab on a plinth, in a grassy mound, engraved with the text.

function graveScene(): Scene {
  let frame: Frame = { t: 0, lookX: 0, lookY: 0, spin: 0 };
  let lines = ["R.I.P."];
  let engraved = false;
  const FACE = 0.1;
  const slab = (x: number, y: number, z: number) => {
    let d = Math.min(sdBox(x, y + 0.13, z, 0.5, 0.5, FACE), sdCylX(z, x, y - 0.37, 0.5, FACE));
    // A chip out of the shoulder.
    return Math.max(d, -(Math.hypot(x - 0.43, y - 0.72, z - 0.1) - 0.13));
  };
  const plinth = (x: number, y: number, z: number) => sdBox(x, y + 0.7, z, 0.62, 0.08, 0.17);
  const mound = (x: number, y: number, z: number) => sdEllipsoid(x, y + 0.86, z - 0.12, 0.98, 0.17, 0.55);
  const sdf = (x: number, y: number, z: number) => Math.min(slab(x, y, z), plinth(x, y, z), mound(x, y, z));
  const G_GRASS = [",", "'", "v", "."].map(glyph);
  return {
    radius: 1.3,
    fit: [0.98, 1.0],
    pose(f) {
      frame = f;
      engraved = !!f.text;
      lines = f.text ? f.text.toUpperCase().split("\n") : ["R.I.P."];
      // No lean or tilt about the view axis: that would shear the lettering off the grid.
      return mul(rotY(0.18 * Math.sin(f.t * 0.35) + f.lookX * 0.28), rotX(0.1 - f.lookY * 0.14));
    },
    intersect(ox, oy, oz, dx, dy, dz, t0, t1, h) {
      if (!march(sdf, ox, oy, oz, dx, dy, dz, t0, t1, h)) return false;
      h.mat = SURFACE;
      if (mound(h.x, h.y, h.z) < 0.004) {
        // Turf: tufts of grass.
        h.value = 0.8;
        const n = Math.floor(noise3(h.x * 14, h.y * 14, h.z * 14) * 7);
        h.glyph = n < G_GRASS.length ? G_GRASS[n] : 0;
        return true;
      }
      // Weathered stone, lichen in patches.
      h.value = 0.92 - 0.3 * clamp((noise3(h.x * 3, h.y * 3, h.z * 3) - 0.55) * 3);
      if (h.z > FACE - 0.01 && Math.abs(h.x) < 0.46) {
        const g = lettering(lines, h.x, h.y, 0.52, frame);
        if (g) {
          h.glyph = g;
          // Someone's name is picked out in red; a plain R.I.P. is just cut. Either way the
          // letters are inked solid, not shaded.
          h.tint = engraved ? BLOOD : INK;
          h.value = 0;
        }
      }
      return true;
    },
  };
}

// ---------------------------------------------------------------------------------------------
// Cauldron: an iron pot on three legs, a blood-red brew that bubbles, and fumes curling up.

function cauldronScene(): Scene {
  let time = 0;
  const BREW = 0.24;
  const BUBBLES = [
    [0.18, 0.1],
    [-0.25, 0.15],
    [0.05, -0.28],
    [-0.12, -0.05],
    [0.3, -0.2],
  ];
  const pot = (x: number, y: number, z: number) => {
    const sphere = Math.hypot(x, y + 0.1, z) - 0.75;
    let d = Math.max(Math.abs(sphere + 0.04) - 0.04, y - 0.38);
    d = Math.min(d, sdTorusY(x, y - 0.38, z, 0.58, 0.065), sdTorusZ(Math.abs(x) - 0.8, y - 0.14, z, 0.11, 0.03));
    for (const a of [0.5, 2.6, 4.7]) {
      const c = Math.cos(a), s = Math.sin(a);
      d = Math.min(d, sdCapsule(x, y, z, c * 0.42, -0.7, s * 0.42, c * 0.54, -0.98, s * 0.54, 0.06));
    }
    return d;
  };
  const surface = (x: number, z: number) => BREW + 0.025 * Math.sin(x * 9 + time * 3) * Math.sin(z * 8 - time * 2.3);
  const brew = (x: number, y: number, z: number) => Math.max(Math.hypot(x, z) - 0.66, Math.abs(y - surface(x, z)) - 0.015) * 0.8;
  const bubbles = (x: number, y: number, z: number) => {
    let d = 1e9;
    BUBBLES.forEach(([bx, bz], i) => {
      const k = (time * 0.55 + i * 0.37) % 1;
      d = Math.min(d, Math.hypot(x - bx, y - BREW - k * 0.22, z - bz) - 0.075 * Math.sin(k * Math.PI));
    });
    return d;
  };
  const fumes = (x: number, y: number, z: number) => smoke(x, y, z, 0, BREW + 0.3, 0, time, 4, 0.42, 0.22);
  const sdf = (x: number, y: number, z: number) => Math.min(pot(x, y, z), brew(x, y, z), bubbles(x, y, z), fumes(x, y, z));
  return {
    radius: 1.3,
    fit: [1.0, 1.02],
    maxInk: 0.85,
    pose(f) {
      time = f.t;
      return mul(rotX(0.64 - f.lookY * 0.2), rotY(f.t * 0.12 + f.lookX * 0.5 + f.spin));
    },
    intersect(ox, oy, oz, dx, dy, dz, t0, t1, h) {
      if (!march(sdf, ox, oy, oz, dx, dy, dz, t0, t1, h)) return false;
      h.mat = SURFACE;
      if (bubbles(h.x, h.y, h.z) < 0.004) {
        h.mat = GLOW;
        h.value = 0.92;
      } else if (brew(h.x, h.y, h.z) < 0.004) {
        h.mat = GLOW;
        h.value = 0.35 + 0.45 * noise3(h.x * 5, h.z * 5, time * 1.2);
      } else if (fumes(h.x, h.y, h.z) < 0.004) {
        h.value = 0.75;
        h.glyph = G_WISP;
      } else {
        h.value = 0.74; // black iron
      }
      return true;
    },
    light(h, lx, ly, lz) {
      // The cursor lantern, plus the brew's glow on the inside of the pot and its rim.
      const diff = Math.max(0, h.nx * lx + h.ny * ly + h.nz * lz);
      const gx = -h.x, gy = BREW - h.y, gz = -h.z, gl = Math.hypot(gx, gy, gz) || 1;
      const glow = Math.max(0, (h.nx * gx + h.ny * gy + h.nz * gz) / gl) * (0.5 / (0.4 + gl * gl * 3));
      return clamp(h.value * (0.1 + 0.9 * diff) + glow);
    },
  };
}

// ---------------------------------------------------------------------------------------------
// Black cat, the agents' Familiar: sitting, tail curled round its paws, head turning to watch
// the cursor with glowing slit-pupilled eyes. It blinks now and then.

function catScene(): Scene {
  let cy = 1, sy = 0, cp = 1, sp = 0, sweep = 0, open = 1, pupil = 0;
  const NY = 0.28, NZ = 0.05;
  const hp = new Float64Array(3);
  /** Into head space: undo the head's turn (yaw) and nod (pitch) about the neck. */
  const head = (x: number, y: number, z: number) => {
    const py = y - NY, pz = z - NZ;
    const x1 = cy * x - sy * pz, z1 = sy * x + cy * pz;
    hp[0] = x1;
    hp[1] = cp * py - sp * z1 + NY;
    hp[2] = sp * py + cp * z1 + NZ;
    return hp;
  };
  const skull = (x: number, y: number, z: number) => {
    const [hx, hy, hz] = head(x, y, z);
    let d = sdEllipsoid(hx, hy - 0.47, hz - 0.08, 0.3, 0.25, 0.26);
    d = smin(d, sdEllipsoid(hx, hy - 0.38, hz - 0.27, 0.13, 0.085, 0.08), 0.06);
    return Math.min(d, sdRoundCone(Math.abs(hx), hy, hz, 0.15, 0.6, 0.06, 0.25, 0.86, 0.03, 0.085, 0.012));
  };
  const TAIL = [
    [0.3, -0.86, -0.12],
    [0.58, -0.88, 0.1],
    [0.66, -0.78, 0.34],
    [0.5, -0.74, 0.5],
  ];
  const body = (x: number, y: number, z: number) => {
    let d = smin(sdEllipsoid(x, y + 0.5, z + 0.05, 0.45, 0.4, 0.4), sdEllipsoid(x, y + 0.05, z - 0.05, 0.28, 0.42, 0.26), 0.15);
    const ax = Math.abs(x);
    d = smin(d, sdCapsule(ax, y, z, 0.12, -0.25, 0.2, 0.13, -0.84, 0.28, 0.07), 0.05);
    d = Math.min(d, sdEllipsoid(ax - 0.13, y + 0.87, z - 0.33, 0.085, 0.05, 0.1));
    for (let i = 0; i < TAIL.length - 1; i++) {
      const [ax0, ay0, az0] = TAIL[i], [bx0, by0, bz0] = TAIL[i + 1];
      // The tip twitches.
      const w = i === TAIL.length - 2 ? sweep : 0;
      d = smin(d, sdCapsule(x, y, z, ax0, ay0, az0, bx0 + w, by0 + Math.abs(w) * 0.5, bz0, 0.06 - i * 0.008), 0.04);
    }
    return d;
  };
  const sdf = (x: number, y: number, z: number) => smin(body(x, y, z), skull(x, y, z), 0.1);
  return {
    radius: 1.25,
    fit: [0.86, 1.0],
    maxInk: 0.86,
    pose(f) {
      const yaw = f.lookX * 0.7, pitch = f.lookY * 0.3;
      cy = Math.cos(yaw);
      sy = Math.sin(yaw);
      cp = Math.cos(pitch);
      sp = Math.sin(pitch);
      pupil = f.lookX * 0.025;
      sweep = 0.1 * Math.sin(f.t * 1.7) * Math.sin(f.t * 0.6);
      const b = f.t % 5.3;
      open = b < 0.18 ? Math.abs(Math.cos((b / 0.18) * Math.PI)) : 1;
      return mul(rotY(0.12 * Math.sin(f.t * 0.3) + f.lookX * 0.2), rotX(0.08));
    },
    intersect(ox, oy, oz, dx, dy, dz, t0, t1, h) {
      if (!march(sdf, ox, oy, oz, dx, dy, dz, t0, t1, h)) return false;
      h.mat = SURFACE;
      h.value = 0.38; // black fur
      if (skull(h.x, h.y, h.z) < 0.01) {
        const [hx, hy, hz] = head(h.x, h.y, h.z);
        const ex = Math.abs(hx) - 0.11, ey = hy - 0.5;
        if (hz > 0.2 && open > 0.2 && (ex / 0.065) ** 2 + (ey / (0.048 * open)) ** 2 < 1) {
          // Glowing eyes with slit pupils that follow the cursor.
          if (Math.abs(ex - pupil * Math.sign(hx)) < 0.014) h.value = 0.1;
          else {
            h.mat = GLOW;
            h.value = 0.95;
          }
        }
      }
      return true;
    },
  };
}
