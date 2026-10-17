/**
 * The haunted props: a skeleton key, a cratered moon, a jack-o'-lantern, a chamber candle and
 * a bat. Each is a Scene for the ASCII renderer in ./engine. Object space: x right, y up,
 * z toward the viewer, sized to fit within about one unit of the origin.
 */
import {
  clamp,
  GLOW,
  type Hit,
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
  sdTorusY,
  sdTorusZ,
  smin,
  SURFACE,
} from "./engine";

export type PropKind = "key" | "moon" | "pumpkin" | "candle" | "bat";

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
  }
}

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

function face(x: number, y: number) {
  const ax = Math.abs(x);
  // Eyes (mirrored) and nose: triangles, wound clockwise.
  let d = tri(ax, y, 0.13, 0.1, 0.33, 0.4, 0.52, 0.1);
  d = Math.min(d, tri(x, y, -0.09, -0.09, 0, 0.06, 0.09, -0.09));
  // A wide grin with two teeth.
  let m = Math.max(y - (-0.19 + 0.5 * x * x), -0.43 + 0.78 * x * x - y, ax - 0.6);
  m = Math.max(m, -box2(x, y, -0.17, -0.2, 0.065, 0.085), -box2(x, y, 0.17, -0.39, 0.065, 0.085));
  return Math.min(d, m * 0.8);
}

function pumpkinScene(): Scene {
  let flicker = 1;
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
    const carved = nearFace ? Math.max(shell, -Math.max(face(x, y), -z)) : shell;
    // The stem: a stubby, bent, tapering capsule.
    const bend = (y - 0.55) * (y - 0.55) * 1.4;
    const stem = sdCapsule(x - bend, y, z, 0, 0.5, 0, 0.02, 0.86, 0, 0.12 - Math.max(0, y - 0.55) * 0.12);
    return smin(carved, stem, 0.04);
  };
  return {
    radius: 1.08,
    fit: [1.05, 1.02],
    pose(f) {
      flicker = 0.78 + 0.22 * noise3(f.t * 6, 0.5, 0.5) + 0.08 * Math.sin(f.t * 23);
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
// Chamber candle: dripping wax in a dish with a ring handle; the flame is the light.

function candleScene(): Scene {
  let sway = 0, tall = 1, flicker = 1;
  const FY = 0.58;
  const wax = (x: number, y: number, z: number) => {
    let d = sdCylY(x, y + 0.2, z, 0.22, 0.6);
    // Melted, slightly cupped top.
    d = Math.max(d, -(Math.hypot(x, y - 0.62, z) - 0.24));
    for (const [a, len] of [
      [0.4, 0.32],
      [1.9, 0.18],
      [2.9, 0.45],
      [4.3, 0.26],
      [5.4, 0.12],
    ]) {
      const px = Math.cos(a) * 0.215, pz = Math.sin(a) * 0.215;
      d = smin(d, sdCapsule(x, y, z, px, 0.36, pz, px, 0.36 - len, pz, 0.035), 0.03);
    }
    return d;
  };
  const dish = (x: number, y: number, z: number) =>
    Math.min(sdCylY(x, y + 0.84, z, 0.56, 0.025), sdTorusY(x, y + 0.81, z, 0.55, 0.035), sdTorusZ(x - 0.66, y + 0.8, z, 0.11, 0.028));
  const flame = (x: number, y: number, z: number) => {
    const yy = (y - FY) / tall;
    const xx = x - sway * Math.max(0, yy + 0.1);
    // Teardrop: an ellipsoid whose top narrows to a point.
    const pinch = 1 + Math.max(0, yy) * 5.5;
    return (Math.hypot(xx * pinch, yy * 0.62, z * pinch) - 0.085) * 0.6;
  };
  const sdf = (x: number, y: number, z: number) => Math.min(wax(x, y, z), dish(x, y, z), flame(x, y, z), sdCylY(x, y - 0.46, z, 0.012, 0.06));
  return {
    radius: 1.05,
    fit: [0.82, 0.98],
    pose(f) {
      flicker = 0.85 + 0.15 * noise3(f.t * 5, 1.5, 0);
      sway = (noise3(f.t * 1.7, 3, 0) - 0.5) * 0.5 - f.lookX * 0.15;
      tall = 0.9 + 0.25 * noise3(f.t * 3.1, 7, 0);
      return mul(rotX(0.32 - f.lookY * 0.2), rotY(f.t * 0.25 + f.lookX * 0.5 + f.spin));
    },
    intersect(ox, oy, oz, dx, dy, dz, t0, t1, h) {
      if (!march(sdf, ox, oy, oz, dx, dy, dz, t0, t1, h)) return false;
      if (flame(h.x, h.y, h.z) < 0.004) {
        h.mat = GLOW;
        // Hotter (ember) at the root of the flame.
        h.value = clamp((1.05 - Math.max(0, h.y - FY) * 2.2) * flicker);
      } else {
        h.mat = SURFACE;
        h.value = h.y < -0.76 ? 0.7 : 0.98;
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

