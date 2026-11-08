// Renders the approved app icon designs (canvas "Search App Icon Concepts",
// page Green Rim: "Approved set", Glass) as 1024 x 1024 layers for Icon
// Composer, rendered to PNG by headless Chrome.
//
// TEMPLATE and Component.renderVals are copied verbatim from the canvas's
// NeuIcon artboard (border variant 6, Green Rim, `glass` on, full-size
// artwork). Each design and appearance is split the way Apple asks for an
// app icon: the opaque ground becomes the icon's fill colour, and every
// part above it is a foreground layer on a transparent page, so macOS can
// draw its clear and tinted styles from them. The canvas's rounded mask,
// hairline frame and glass tile edge are previews of what macOS draws, so
// they are left out.
//
// usage: node scripts/icons/neu-icon.mjs OUT_DIR
// Writes OUT_DIR/<slug>-<light|dark>.html (the foreground) for the six
// designs, and OUT_DIR/fills.txt: one "<slug>-<light|dark> <#rrggbb>" line
// per page with its ground colour. Run by scripts/build-icons.sh; refresh
// TEMPLATE and renderVals from the canvas when the designs change.

import { mkdirSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';

// The approved set in canvas order: the NeuIcon `icon` number and the slug
// that fx_ui_macos::app_icon::AppIcon uses in file names and settings.
export const DESIGNS = [
  { n: 1, slug: 'soft-lens' },
  { n: 2, slug: 'pressed-lens' },
  { n: 3, slug: 'match-row' },
  { n: 4, slug: 'glow-row' },
  { n: 5, slug: 'front-stack' },
  { n: 6, slug: 'lit-slot' },
];

// NeuIcon's template from its root element's children: the shared defs, then
// one `sc-if` block per design.
const TEMPLATE = String.raw`<svg width="0" height="0" aria-hidden="true" focusable="false" style="position: absolute">
<defs>
<linearGradient id="{{id.face}}" x1="0" y1="0" x2="1" y2="1"><stop offset="0" stop-color="{{c.f0}}" stop-opacity="{{c.f0O}}"></stop><stop offset="1" stop-color="{{c.f1}}" stop-opacity="{{c.f1O}}"></stop></linearGradient>
<radialGradient id="{{id.accR}}" cx="0.4" cy="0.35" r="0.8"><stop offset="0" stop-color="{{c.g0}}" stop-opacity="{{c.g0O}}"></stop><stop offset="0.55" stop-color="{{c.gm}}" stop-opacity="{{c.gmO}}"></stop><stop offset="1" stop-color="{{c.g1}}" stop-opacity="{{c.g1O}}"></stop></radialGradient>
<linearGradient id="{{id.accL}}" x1="0" y1="0" x2="1" y2="1"><stop offset="0" stop-color="{{c.g0}}" stop-opacity="{{c.g0O}}"></stop><stop offset="1" stop-color="{{c.g1}}" stop-opacity="{{c.g1O}}"></stop></linearGradient>
<linearGradient id="{{id.bvOut}}" x1="0" y1="0" x2="1" y2="1"><stop offset="0" stop-color="{{e.hi}}" stop-opacity="{{e.hiO}}"></stop><stop offset="1" stop-color="{{e.lo}}" stop-opacity="{{e.loO}}"></stop></linearGradient>
<linearGradient id="{{id.bvIn}}" x1="0" y1="0" x2="1" y2="1"><stop offset="0" stop-color="{{e.lo}}" stop-opacity="{{e.loO}}"></stop><stop offset="1" stop-color="{{e.hi}}" stop-opacity="{{e.hiO}}"></stop></linearGradient>
<linearGradient id="{{id.bvG}}" x1="0" y1="0" x2="1" y2="1"><stop offset="0" stop-color="{{e.ghi}}" stop-opacity="{{e.ghiO}}"></stop><stop offset="1" stop-color="{{e.glo}}" stop-opacity="{{e.gloO}}"></stop></linearGradient>
<filter id="{{id.up}}" filterUnits="userSpaceOnUse" x="0" y="0" width="1024" height="1024">
<feGaussianBlur in="SourceAlpha" stdDeviation="{{s.ob}}" result="b"></feGaussianBlur>
<feOffset in="b" dx="{{s.od}}" dy="{{s.od}}" result="bd"></feOffset>
<feFlood flood-color="{{c.dk}}" flood-opacity="{{c.dkO}}"></feFlood>
<feComposite in2="bd" operator="in" result="dk"></feComposite>
<feOffset in="b" dx="{{s.ol}}" dy="{{s.ol}}" result="bl"></feOffset>
<feFlood flood-color="{{c.lt}}" flood-opacity="{{c.ltO}}"></feFlood>
<feComposite in2="bl" operator="in" result="lt"></feComposite>
<feMorphology in="SourceAlpha" operator="dilate" radius="6" result="dg"></feMorphology>
<feGaussianBlur in="dg" stdDeviation="14" result="bg"></feGaussianBlur>
<feFlood flood-color="{{c.glow}}" flood-opacity="{{e.glowO}}"></feFlood>
<feComposite in2="bg" operator="in" result="gl"></feComposite>
<feMerge><feMergeNode in="dk"></feMergeNode><feMergeNode in="lt"></feMergeNode><feMergeNode in="gl"></feMergeNode><feMergeNode in="SourceGraphic"></feMergeNode></feMerge>
</filter>
<filter id="{{id.inn}}" filterUnits="userSpaceOnUse" x="0" y="0" width="1024" height="1024">
<feOffset in="SourceAlpha" dx="{{s.io}}" dy="{{s.io}}" result="o1"></feOffset>
<feGaussianBlur in="o1" stdDeviation="{{s.ib}}" result="b1"></feGaussianBlur>
<feComposite in="SourceAlpha" in2="b1" operator="out" result="e1"></feComposite>
<feFlood flood-color="{{c.idk}}" flood-opacity="{{c.idkO}}"></feFlood>
<feComposite in2="e1" operator="in" result="s1"></feComposite>
<feOffset in="SourceAlpha" dx="{{s.ion}}" dy="{{s.ion}}" result="o2"></feOffset>
<feGaussianBlur in="o2" stdDeviation="{{s.ib}}" result="b2"></feGaussianBlur>
<feComposite in="SourceAlpha" in2="b2" operator="out" result="e2"></feComposite>
<feFlood flood-color="{{c.ilt}}" flood-opacity="{{c.iltO}}"></feFlood>
<feComposite in2="e2" operator="in" result="s2"></feComposite>
<feMerge><feMergeNode in="SourceGraphic"></feMergeNode><feMergeNode in="s1"></feMergeNode><feMergeNode in="s2"></feMergeNode></feMerge>
</filter>
<filter id="{{id.innG}}" filterUnits="userSpaceOnUse" x="0" y="0" width="1024" height="1024">
<feOffset in="SourceAlpha" dx="{{s.io}}" dy="{{s.io}}" result="o1"></feOffset>
<feGaussianBlur in="o1" stdDeviation="{{s.ib}}" result="b1"></feGaussianBlur>
<feComposite in="SourceAlpha" in2="b1" operator="out" result="e1"></feComposite>
<feFlood flood-color="{{c.idkG}}" flood-opacity="{{c.idkGO}}"></feFlood>
<feComposite in2="e1" operator="in" result="s1"></feComposite>
<feOffset in="SourceAlpha" dx="{{s.ion}}" dy="{{s.ion}}" result="o2"></feOffset>
<feGaussianBlur in="o2" stdDeviation="{{s.ib}}" result="b2"></feGaussianBlur>
<feComposite in="SourceAlpha" in2="b2" operator="out" result="e2"></feComposite>
<feFlood flood-color="{{c.iltG}}" flood-opacity="{{c.iltGO}}"></feFlood>
<feComposite in2="e2" operator="in" result="s2"></feComposite>
<feMerge><feMergeNode in="SourceGraphic"></feMergeNode><feMergeNode in="s1"></feMergeNode><feMergeNode in="s2"></feMergeNode></feMerge>
</filter>
<filter id="{{id.soft}}" filterUnits="userSpaceOnUse" x="0" y="0" width="1024" height="1024"><feGaussianBlur stdDeviation="{{e.sb}}"></feGaussianBlur></filter>
<filter id="{{id.glow}}" filterUnits="userSpaceOnUse" x="0" y="0" width="1024" height="1024"><feGaussianBlur stdDeviation="12"></feGaussianBlur></filter>
</defs>
</svg>

<sc-if value="{{is1}}" hint-placeholder-val="{{ true }}">
<svg viewBox="0 0 1024 1024" width="100%" height="100%" aria-hidden="true">
<rect width="1024" height="1024" fill="{{c.bg}}" fill-opacity="{{c.bgO}}"></rect>
<g filter="url(#{{id.up}})">
<g fill="none" stroke="{{e.rim}}" stroke-width="{{e.rimW}}" opacity="{{e.rimO}}">
<circle cx="416" cy="416" r="{{g.rimOut}}"></circle>
<rect x="{{g.H.rm.x}}" y="{{g.H.rm.y}}" width="{{g.H.rm.w}}" height="{{g.H.rm.h}}" rx="{{g.H.rm.r}}" transform="translate(416 416) rotate(45)"></rect>
</g>
<g filter="url(#{{id.soft}})" fill="none" stroke="url(#{{id.bvOut}})" stroke-width="{{e.w}}">
<circle cx="416" cy="416" r="{{g.bandOut}}"></circle>
<rect x="{{g.H.bv.x}}" y="{{g.H.bv.y}}" width="{{g.H.bv.w}}" height="{{g.H.bv.h}}" rx="{{g.H.bv.r}}" transform="translate(416 416) rotate(45)"></rect>
</g>
<circle cx="416" cy="416" r="176" fill="none" stroke="url(#{{id.face}})" stroke-width="96"></circle>
<rect x="210" y="-60" width="360" height="120" rx="60" fill="url(#{{id.face}})" transform="translate(416 416) rotate(45)"></rect>
</g>
<circle cx="416" cy="416" r="{{g.rimIn}}" fill="none" stroke="{{e.rim}}" stroke-width="{{e.rimW}}" opacity="{{e.rimO}}"></circle>
<g filter="url(#{{id.soft}})"><circle cx="416" cy="416" r="{{g.bandIn}}" fill="none" stroke="url(#{{id.bvIn}})" stroke-width="{{e.w}}"></circle></g>
<circle cx="416" cy="416" r="{{g.well}}" fill="url(#{{id.accR}})" filter="url(#{{id.innG}})"></circle>
</svg>
</sc-if>

<sc-if value="{{is2}}" hint-placeholder-val="{{ false }}">
<svg viewBox="0 0 1024 1024" width="100%" height="100%" aria-hidden="true">
<rect width="1024" height="1024" fill="{{c.bg}}" fill-opacity="{{c.bgO}}"></rect>
<g fill="none" stroke="{{e.rim}}" stroke-width="{{e.rimW}}" opacity="{{e.rimO}}">
<circle cx="416" cy="416" r="{{g.rimOut}}"></circle>
<rect x="{{g.H.rm.x}}" y="{{g.H.rm.y}}" width="{{g.H.rm.w}}" height="{{g.H.rm.h}}" rx="{{g.H.rm.r}}" transform="translate(416 416) rotate(45)"></rect>
</g>
<g filter="url(#{{id.soft}})" fill="none" stroke="url(#{{id.bvIn}})" stroke-width="{{e.w}}">
<circle cx="416" cy="416" r="{{g.bandOut}}"></circle>
<rect x="{{g.H.bv.x}}" y="{{g.H.bv.y}}" width="{{g.H.bv.w}}" height="{{g.H.bv.h}}" rx="{{g.H.bv.r}}" transform="translate(416 416) rotate(45)"></rect>
</g>
<g filter="url(#{{id.inn}})">
<circle cx="416" cy="416" r="176" fill="none" stroke="{{c.pit}}" stroke-opacity="{{c.pitO}}" stroke-width="96"></circle>
<rect x="210" y="-60" width="360" height="120" rx="60" fill="{{c.pit}}" fill-opacity="{{c.pitO}}" transform="translate(416 416) rotate(45)"></rect>
</g>
<circle cx="416" cy="416" r="{{g.rimIn}}" fill="none" stroke="{{e.rim}}" stroke-width="{{e.rimW}}" opacity="{{e.rimO}}"></circle>
<g filter="url(#{{id.soft}})"><circle cx="416" cy="416" r="{{g.bandIn}}" fill="none" stroke="url(#{{id.bvOut}})" stroke-width="{{e.w}}"></circle></g>
<g opacity="{{e.ledGlowO}}" filter="url(#{{id.glow}})" fill="none" stroke="{{c.led}}" stroke-width="36" stroke-linecap="round">
<circle cx="416" cy="416" r="176"></circle>
<path d="M 236 0 H 530" transform="translate(416 416) rotate(45)"></path>
</g>
<g fill="none" stroke="{{c.led}}" stroke-opacity="{{c.ledO}}" stroke-width="{{g.ledW}}" stroke-linecap="round">
<circle cx="416" cy="416" r="176"></circle>
<path d="M 236 0 H 530" transform="translate(416 416) rotate(45)"></path>
</g>
<g filter="url(#{{id.up}})">
<circle cx="416" cy="416" r="{{g.discRim}}" fill="none" stroke="{{e.rim}}" stroke-width="{{e.rimW}}" opacity="{{e.rimO}}"></circle>
<g filter="url(#{{id.soft}})"><circle cx="416" cy="416" r="{{g.discBand}}" fill="none" stroke="url(#{{id.bvOut}})" stroke-width="{{e.w}}"></circle></g>
<circle cx="416" cy="416" r="{{g.disc}}" fill="url(#{{id.face}})"></circle>
</g>
</svg>
</sc-if>

<sc-if value="{{is3}}" hint-placeholder-val="{{ false }}">
<svg viewBox="0 0 1024 1024" width="100%" height="100%" aria-hidden="true">
<rect width="1024" height="1024" fill="{{c.bg}}" fill-opacity="{{c.bgO}}"></rect>
<rect x="{{g.Q.rm.x}}" y="{{g.Q.rm.y}}" width="{{g.Q.rm.w}}" height="{{g.Q.rm.h}}" rx="{{g.Q.rm.r}}" fill="none" stroke="{{e.rim}}" stroke-width="{{e.rimW}}" opacity="{{e.rimO}}"></rect>
<rect x="{{g.Q.bv.x}}" y="{{g.Q.bv.y}}" width="{{g.Q.bv.w}}" height="{{g.Q.bv.h}}" rx="{{g.Q.bv.r}}" fill="none" stroke="url(#{{id.bvIn}})" stroke-width="{{e.w}}" filter="url(#{{id.soft}})"></rect>
<rect x="196" y="222" width="632" height="112" rx="56" fill="{{c.pit}}" fill-opacity="{{c.pitO}}" filter="url(#{{id.inn}})"></rect>
<rect x="252" y="264" width="220" height="28" rx="14" fill="{{c.ink}}" fill-opacity="{{c.inkO}}"></rect>
<rect x="490" y="250" width="14" height="56" rx="7" fill="{{c.gm}}" fill-opacity="{{c.gmO}}"></rect>
<g filter="url(#{{id.up}})">
<rect x="{{g.B1.rm.x}}" y="{{g.B1.rm.y}}" width="{{g.B1.rm.w}}" height="{{g.B1.rm.h}}" rx="{{g.B1.rm.r}}" fill="none" stroke="{{e.rim}}" stroke-width="{{e.rimW}}" opacity="{{e.rimO}}"></rect>
<rect x="{{g.B1.bv.x}}" y="{{g.B1.bv.y}}" width="{{g.B1.bv.w}}" height="{{g.B1.bv.h}}" rx="{{g.B1.bv.r}}" fill="none" stroke="url(#{{id.bvOut}})" stroke-width="{{e.w}}" filter="url(#{{id.soft}})"></rect>
<rect x="196" y="404" width="520" height="92" rx="46" fill="url(#{{id.face}})"></rect>
</g>
<g filter="url(#{{id.up}})">
<rect x="{{g.B2.rm.x}}" y="{{g.B2.rm.y}}" width="{{g.B2.rm.w}}" height="{{g.B2.rm.h}}" rx="{{g.B2.rm.r}}" fill="none" stroke="{{e.rim}}" stroke-width="{{e.rimW}}" opacity="{{e.rimO}}"></rect>
<rect x="{{g.B2.bv.x}}" y="{{g.B2.bv.y}}" width="{{g.B2.bv.w}}" height="{{g.B2.bv.h}}" rx="{{g.B2.bv.r}}" fill="none" stroke="url(#{{id.bvG}})" stroke-width="{{e.w}}" filter="url(#{{id.soft}})"></rect>
<rect x="196" y="556" width="632" height="92" rx="46" fill="url(#{{id.accL}})"></rect>
</g>
<g filter="url(#{{id.up}})">
<rect x="{{g.B3.rm.x}}" y="{{g.B3.rm.y}}" width="{{g.B3.rm.w}}" height="{{g.B3.rm.h}}" rx="{{g.B3.rm.r}}" fill="none" stroke="{{e.rim}}" stroke-width="{{e.rimW}}" opacity="{{e.rimO}}"></rect>
<rect x="{{g.B3.bv.x}}" y="{{g.B3.bv.y}}" width="{{g.B3.bv.w}}" height="{{g.B3.bv.h}}" rx="{{g.B3.bv.r}}" fill="none" stroke="url(#{{id.bvOut}})" stroke-width="{{e.w}}" filter="url(#{{id.soft}})"></rect>
<rect x="196" y="708" width="420" height="92" rx="46" fill="url(#{{id.face}})"></rect>
</g>
</svg>
</sc-if>

<sc-if value="{{is4}}" hint-placeholder-val="{{ false }}">
<svg viewBox="0 0 1024 1024" width="100%" height="100%" aria-hidden="true">
<rect width="1024" height="1024" fill="{{c.bg}}" fill-opacity="{{c.bgO}}"></rect>
<g filter="url(#{{id.up}})">
<rect x="{{g.Q.rm.x}}" y="{{g.Q.rm.y}}" width="{{g.Q.rm.w}}" height="{{g.Q.rm.h}}" rx="{{g.Q.rm.r}}" fill="none" stroke="{{e.rim}}" stroke-width="{{e.rimW}}" opacity="{{e.rimO}}"></rect>
<rect x="{{g.Q.bv.x}}" y="{{g.Q.bv.y}}" width="{{g.Q.bv.w}}" height="{{g.Q.bv.h}}" rx="{{g.Q.bv.r}}" fill="none" stroke="url(#{{id.bvOut}})" stroke-width="{{e.w}}" filter="url(#{{id.soft}})"></rect>
<rect x="196" y="222" width="632" height="112" rx="56" fill="url(#{{id.face}})"></rect>
</g>
<rect x="252" y="264" width="220" height="28" rx="14" fill="{{c.ink}}" fill-opacity="{{c.inkO}}"></rect>
<rect x="490" y="250" width="14" height="56" rx="7" fill="{{c.gm}}" fill-opacity="{{c.gmO}}"></rect>
<g filter="url(#{{id.up}})">
<rect x="{{g.B1.rm.x}}" y="{{g.B1.rm.y}}" width="{{g.B1.rm.w}}" height="{{g.B1.rm.h}}" rx="{{g.B1.rm.r}}" fill="none" stroke="{{e.rim}}" stroke-width="{{e.rimW}}" opacity="{{e.rimO}}"></rect>
<rect x="{{g.B1.bv.x}}" y="{{g.B1.bv.y}}" width="{{g.B1.bv.w}}" height="{{g.B1.bv.h}}" rx="{{g.B1.bv.r}}" fill="none" stroke="url(#{{id.bvOut}})" stroke-width="{{e.w}}" filter="url(#{{id.soft}})"></rect>
<rect x="196" y="404" width="520" height="92" rx="46" fill="url(#{{id.face}})"></rect>
</g>
<rect x="{{g.B2.rm.x}}" y="{{g.B2.rm.y}}" width="{{g.B2.rm.w}}" height="{{g.B2.rm.h}}" rx="{{g.B2.rm.r}}" fill="none" stroke="{{e.rim}}" stroke-width="{{e.rimW}}" opacity="{{e.rimO}}"></rect>
<rect x="{{g.B2.bv.x}}" y="{{g.B2.bv.y}}" width="{{g.B2.bv.w}}" height="{{g.B2.bv.h}}" rx="{{g.B2.bv.r}}" fill="none" stroke="url(#{{id.bvIn}})" stroke-width="{{e.w}}" filter="url(#{{id.soft}})"></rect>
<rect x="196" y="556" width="632" height="92" rx="46" fill="{{c.pit}}" fill-opacity="{{c.pitO}}" filter="url(#{{id.inn}})"></rect>
<rect x="224" y="586" width="576" height="32" rx="16" fill="{{c.led}}" opacity="{{e.ledGlowO}}" filter="url(#{{id.glow}})"></rect>
<rect x="224" y="586" width="576" height="32" rx="16" fill="{{c.led}}" fill-opacity="{{c.ledO}}"></rect>
<g filter="url(#{{id.up}})">
<rect x="{{g.B3.rm.x}}" y="{{g.B3.rm.y}}" width="{{g.B3.rm.w}}" height="{{g.B3.rm.h}}" rx="{{g.B3.rm.r}}" fill="none" stroke="{{e.rim}}" stroke-width="{{e.rimW}}" opacity="{{e.rimO}}"></rect>
<rect x="{{g.B3.bv.x}}" y="{{g.B3.bv.y}}" width="{{g.B3.bv.w}}" height="{{g.B3.bv.h}}" rx="{{g.B3.bv.r}}" fill="none" stroke="url(#{{id.bvOut}})" stroke-width="{{e.w}}" filter="url(#{{id.soft}})"></rect>
<rect x="196" y="708" width="420" height="92" rx="46" fill="url(#{{id.face}})"></rect>
</g>
</svg>
</sc-if>

<sc-if value="{{is5}}" hint-placeholder-val="{{ false }}">
<svg viewBox="0 0 1024 1024" width="100%" height="100%" aria-hidden="true">
<rect width="1024" height="1024" fill="{{c.bg}}" fill-opacity="{{c.bgO}}"></rect>
<g filter="url(#{{id.up}})">
<rect x="{{g.C1.rm.x}}" y="{{g.C1.rm.y}}" width="{{g.C1.rm.w}}" height="{{g.C1.rm.h}}" rx="{{g.C1.rm.r}}" fill="none" stroke="{{e.rim}}" stroke-width="{{e.rimW}}" opacity="{{e.rimO}}"></rect>
<rect x="{{g.C1.bv.x}}" y="{{g.C1.bv.y}}" width="{{g.C1.bv.w}}" height="{{g.C1.bv.h}}" rx="{{g.C1.bv.r}}" fill="none" stroke="url(#{{id.bvG}})" stroke-width="{{e.w}}" filter="url(#{{id.soft}})"></rect>
<rect x="150" y="176" width="520" height="104" rx="52" fill="url(#{{id.accL}})"></rect>
</g>
<g filter="url(#{{id.up}})">
<rect x="{{g.C2.rm.x}}" y="{{g.C2.rm.y}}" width="{{g.C2.rm.w}}" height="{{g.C2.rm.h}}" rx="{{g.C2.rm.r}}" fill="none" stroke="{{e.rim}}" stroke-width="{{e.rimW}}" opacity="{{e.rimO}}"></rect>
<rect x="{{g.C2.bv.x}}" y="{{g.C2.bv.y}}" width="{{g.C2.bv.w}}" height="{{g.C2.bv.h}}" rx="{{g.C2.bv.r}}" fill="none" stroke="url(#{{id.bvOut}})" stroke-width="{{e.w}}" filter="url(#{{id.soft}})"></rect>
<rect x="150" y="322" width="520" height="104" rx="52" fill="url(#{{id.face}})"></rect>
</g>
<g filter="url(#{{id.up}})">
<rect x="{{g.C3.rm.x}}" y="{{g.C3.rm.y}}" width="{{g.C3.rm.w}}" height="{{g.C3.rm.h}}" rx="{{g.C3.rm.r}}" fill="none" stroke="{{e.rim}}" stroke-width="{{e.rimW}}" opacity="{{e.rimO}}"></rect>
<rect x="{{g.C3.bv.x}}" y="{{g.C3.bv.y}}" width="{{g.C3.bv.w}}" height="{{g.C3.bv.h}}" rx="{{g.C3.bv.r}}" fill="none" stroke="url(#{{id.bvOut}})" stroke-width="{{e.w}}" filter="url(#{{id.soft}})"></rect>
<rect x="150" y="468" width="520" height="104" rx="52" fill="url(#{{id.face}})"></rect>
</g>
<g filter="url(#{{id.up}})">
<g fill="none" stroke="{{e.rim}}" opacity="{{e.rimO}}" stroke-linejoin="round">
<circle cx="600" cy="576" r="{{g.L.rimOut}}" stroke-width="{{e.rimW}}"></circle>
<path d="M 758.4 669.3 L 848.9 759.8 L 848.9 807.9 L 807.9 848.9 L 693.3 734.4 Z" stroke-width="{{g.L.hsR}}"></path>
</g>
<g filter="url(#{{id.soft}})" fill="none" stroke="url(#{{id.bvOut}})" stroke-linejoin="round">
<circle cx="600" cy="576" r="{{g.L.bandOut}}" stroke-width="{{e.w}}"></circle>
<path d="M 758.4 669.3 L 848.9 759.8 L 848.9 807.9 L 807.9 848.9 L 693.3 734.4 Z" stroke-width="{{g.L.hsB}}"></path>
</g>
<circle cx="600" cy="576" r="128" fill="none" stroke="url(#{{id.face}})" stroke-width="76"></circle>
<path d="M 758.4 669.3 L 848.9 759.8 L 848.9 807.9 L 807.9 848.9 L 693.3 734.4 Z" fill="url(#{{id.face}})" stroke="url(#{{id.face}})" stroke-width="20" stroke-linejoin="round"></path>
</g>
<circle cx="600" cy="576" r="{{g.L.rimIn}}" fill="none" stroke="{{e.rim}}" stroke-width="{{e.rimW}}" opacity="{{e.rimO}}"></circle>
<g filter="url(#{{id.soft}})"><circle cx="600" cy="576" r="{{g.L.bandIn}}" fill="none" stroke="url(#{{id.bvIn}})" stroke-width="{{e.w}}"></circle></g>
<circle cx="600" cy="576" r="{{g.L.glass}}" fill="url(#{{id.accR}})" filter="url(#{{id.innG}})"></circle>
</svg>
</sc-if>

<sc-if value="{{is6}}" hint-placeholder-val="{{ false }}">
<svg viewBox="0 0 1024 1024" width="100%" height="100%" aria-hidden="true">
<rect width="1024" height="1024" fill="{{c.bg}}" fill-opacity="{{c.bgO}}"></rect>
<rect x="{{g.C1.rm.x}}" y="{{g.C1.rm.y}}" width="{{g.C1.rm.w}}" height="{{g.C1.rm.h}}" rx="{{g.C1.rm.r}}" fill="none" stroke="{{e.rim}}" stroke-width="{{e.rimW}}" opacity="{{e.rimO}}"></rect>
<rect x="{{g.C1.bv.x}}" y="{{g.C1.bv.y}}" width="{{g.C1.bv.w}}" height="{{g.C1.bv.h}}" rx="{{g.C1.bv.r}}" fill="none" stroke="url(#{{id.bvIn}})" stroke-width="{{e.w}}" filter="url(#{{id.soft}})"></rect>
<rect x="150" y="176" width="520" height="104" rx="52" fill="{{c.pit}}" fill-opacity="{{c.pitO}}" filter="url(#{{id.inn}})"></rect>
<rect x="182" y="212" width="456" height="32" rx="16" fill="{{c.led}}" opacity="{{e.ledGlowO}}" filter="url(#{{id.glow}})"></rect>
<rect x="182" y="212" width="456" height="32" rx="16" fill="{{c.led}}" fill-opacity="{{c.ledO}}"></rect>
<rect x="{{g.C2.rm.x}}" y="{{g.C2.rm.y}}" width="{{g.C2.rm.w}}" height="{{g.C2.rm.h}}" rx="{{g.C2.rm.r}}" fill="none" stroke="{{e.rim}}" stroke-width="{{e.rimW}}" opacity="{{e.rimO}}"></rect>
<rect x="{{g.C2.bv.x}}" y="{{g.C2.bv.y}}" width="{{g.C2.bv.w}}" height="{{g.C2.bv.h}}" rx="{{g.C2.bv.r}}" fill="none" stroke="url(#{{id.bvIn}})" stroke-width="{{e.w}}" filter="url(#{{id.soft}})"></rect>
<rect x="150" y="322" width="520" height="104" rx="52" fill="{{c.pit}}" fill-opacity="{{c.pitO}}" filter="url(#{{id.inn}})"></rect>
<rect x="{{g.C3.rm.x}}" y="{{g.C3.rm.y}}" width="{{g.C3.rm.w}}" height="{{g.C3.rm.h}}" rx="{{g.C3.rm.r}}" fill="none" stroke="{{e.rim}}" stroke-width="{{e.rimW}}" opacity="{{e.rimO}}"></rect>
<rect x="{{g.C3.bv.x}}" y="{{g.C3.bv.y}}" width="{{g.C3.bv.w}}" height="{{g.C3.bv.h}}" rx="{{g.C3.bv.r}}" fill="none" stroke="url(#{{id.bvIn}})" stroke-width="{{e.w}}" filter="url(#{{id.soft}})"></rect>
<rect x="150" y="468" width="520" height="104" rx="52" fill="{{c.pit}}" fill-opacity="{{c.pitO}}" filter="url(#{{id.inn}})"></rect>
<g filter="url(#{{id.up}})">
<g fill="none" stroke="{{e.rim}}" opacity="{{e.rimO}}" stroke-linejoin="round">
<circle cx="600" cy="576" r="{{g.L.rimOut}}" stroke-width="{{e.rimW}}"></circle>
<path d="M 758.4 669.3 L 848.9 759.8 L 848.9 807.9 L 807.9 848.9 L 693.3 734.4 Z" stroke-width="{{g.L.hsR}}"></path>
</g>
<g filter="url(#{{id.soft}})" fill="none" stroke="url(#{{id.bvOut}})" stroke-linejoin="round">
<circle cx="600" cy="576" r="{{g.L.bandOut}}" stroke-width="{{e.w}}"></circle>
<path d="M 758.4 669.3 L 848.9 759.8 L 848.9 807.9 L 807.9 848.9 L 693.3 734.4 Z" stroke-width="{{g.L.hsB}}"></path>
</g>
<circle cx="600" cy="576" r="128" fill="none" stroke="url(#{{id.face}})" stroke-width="76"></circle>
<path d="M 758.4 669.3 L 848.9 759.8 L 848.9 807.9 L 807.9 848.9 L 693.3 734.4 Z" fill="url(#{{id.face}})" stroke="url(#{{id.face}})" stroke-width="20" stroke-linejoin="round"></path>
</g>
<circle cx="600" cy="576" r="{{g.L.rimIn}}" fill="none" stroke="{{e.rim}}" stroke-width="{{e.rimW}}" opacity="{{e.rimO}}"></circle>
<g filter="url(#{{id.soft}})"><circle cx="600" cy="576" r="{{g.L.bandIn}}" fill="none" stroke="url(#{{id.bvIn}})" stroke-width="{{e.w}}"></circle></g>
<circle cx="600" cy="576" r="{{g.L.glass}}" fill="{{c.pit}}" fill-opacity="{{c.pitO}}" filter="url(#{{id.inn}})"></circle>
<circle cx="600" cy="576" r="{{g.L.led}}" fill="none" stroke="{{c.led}}" stroke-width="22" opacity="{{e.ledGlowO}}" filter="url(#{{id.glow}})"></circle>
<circle cx="600" cy="576" r="{{g.L.led}}" fill="none" stroke="{{c.led}}" stroke-opacity="{{c.ledO}}" stroke-width="{{g.ledW}}"></circle>
</svg>
</sc-if>`;

// NeuIcon's logic, verbatim; `props` are the artboard's props.
class Component {
  constructor(props) {
    this.props = props;
  }

  renderVals() {
    if (!this._uid) this._uid = 'nu' + Math.random().toString(36).slice(2, 10);
    const u = this._uid;
    const clamp = (x, lo, hi, d) => Math.min(hi, Math.max(lo, Math.round(Number(x ?? d)) || d));
    const n = clamp(this.props.icon, 1, 6, 1);
    const vi = clamp(this.props.variant, 1, 10, 1);
    const size = Number(this.props.size ?? 512) || 512;
    const MODES = ['default', 'dark', 'clear-light', 'clear-dark', 'tinted-light', 'tinted-dark', 'glass-light', 'glass-dark'];
    const mode = MODES.includes(this.props.appearance) ? this.props.appearance : 'default';
    const small = this.props.small === true || this.props.small === 'true';
    const glass = this.props.glass === true || this.props.glass === 'true';
    const tint = /^#[0-9a-f]{6}$/i.test(this.props.tint ?? '') ? this.props.tint : '#f58606';
    const ICONS = ['Soft Lens', 'Pressed Lens', 'Match Row', 'Glow Row', 'Front Stack', 'Lit Slot'];
    const NAMES = ['Baseline', 'Hairline', 'Faint', 'Heavy', 'Green Edge', 'Green Rim', 'Ink Outline', 'Charcoal Edge', 'Green Glow', 'Wide Soft'];

    const hex = (h) => [1, 3, 5].map((i) => parseInt(h.slice(i, i + 2), 16));
    const mix = (a, b, t) => '#' + hex(a).map((x, i) => Math.round(x + (hex(b)[i] - x) * t).toString(16).padStart(2, '0')).join('');

    // Which palette each appearance uses. Icons 2, 4 and 6 are dark by default, so their dark
    // appearance goes one step deeper.
    const darkBorn = n === 2 || n === 4 || n === 6;
    const SYS = { 'clear-light': 'CL', 'clear-dark': 'CD', 'tinted-light': 'TL', 'tinted-dark': 'TD', 'glass-light': 'GL', 'glass-dark': 'GD' };
    const base = mode === 'default' ? (darkBorn ? 'D' : 'L') : mode === 'dark' ? (darkBorn ? 'X' : 'D') : SYS[mode];
    // Glass swaps every appearance for its higher-transparency version; clear becomes the Photos-matched glass.
    const key = glass ? ({ L: 'Lg', D: 'Dg', X: 'Xg', CL: 'GL', CD: 'GD', TL: 'TLg', TD: 'TDg' }[base] || base) : base;
    const G = ['#3fdc84', 1];
    const P = {
      L: { bg: ['#d9e1e4', 1], f0: ['#e4eaed', 1], f1: ['#cbd5d9', 1], dk: ['#788991', 1], lt: ['#ffffff', 0.75], pit: ['#d1dade', 1], idk: ['#6f8088', 0.95], ilt: ['#ffffff', 1], idkG: ['#043d1e', 0.9], iltG: ['#c9f7dc', 0.8], g0: G, gm: ['#18b05a', 1], g1: ['#0f8a45', 1], ink: ['#555555', 1], led: ['#18b05a', 1], rim: ['#18b05a', 1], glow: '#18b05a' },
      D: { bg: ['#393939', 1], f0: ['#4a4c4e', 1], f1: ['#303233', 1], dk: ['#121212', 1], lt: ['#535659', 0.9], pit: ['#2e2f30', 1], idk: ['#0a0a0a', 1], ilt: ['#5e6164', 0.9], idkG: ['#021f0f', 0.95], iltG: ['#5fe39a', 0.5], g0: G, gm: ['#18b05a', 1], g1: ['#0f8a45', 1], ink: ['#8d8f93', 1], led: ['#18b05a', 1], rim: ['#18b05a', 1], glow: '#18b05a' },
      X: { bg: ['#161718', 1], f0: ['#2c2e2f', 1], f1: ['#1b1c1d', 1], dk: ['#000000', 1], lt: ['#34363a', 0.8], pit: ['#111213', 1], idk: ['#000000', 1], ilt: ['#3a3c3e', 0.8], idkG: ['#011209', 1], iltG: ['#3fdc84', 0.35], g0: ['#2fc070', 1], gm: ['#15a052', 1], g1: ['#0b6e36', 1], ink: ['#6d6f73', 1], led: ['#15a052', 1], rim: ['#15a052', 1], glow: '#15a052' },
      CL: { bg: ['#ffffff', 0.38], f0: ['#ffffff', 0.72], f1: ['#ffffff', 0.45], dk: ['#000000', 0.18], lt: ['#ffffff', 0.55], pit: ['#ffffff', 0.2], idk: ['#000000', 0.22], ilt: ['#ffffff', 0.7], idkG: ['#000000', 0.22], iltG: ['#ffffff', 0.7], g0: ['#ffffff', 0.98], gm: ['#ffffff', 0.92], g1: ['#ffffff', 0.8], ink: ['#000000', 0.32], led: ['#ffffff', 0.95], rim: ['#ffffff', 0.9], glow: '#ffffff' },
      CD: { bg: ['#000000', 0.45], f0: ['#ffffff', 0.24], f1: ['#ffffff', 0.1], dk: ['#000000', 0.45], lt: ['#ffffff', 0.12], pit: ['#000000', 0.3], idk: ['#000000', 0.5], ilt: ['#ffffff', 0.18], idkG: ['#000000', 0.45], iltG: ['#ffffff', 0.3], g0: ['#ffffff', 0.95], gm: ['#ffffff', 0.85], g1: ['#ffffff', 0.68], ink: ['#ffffff', 0.5], led: ['#ffffff', 0.9], rim: ['#ffffff', 0.8], glow: '#ffffff' },
      TL: { bg: [tint, 1], f0: [mix(tint, '#ffffff', 0.42), 1], f1: [mix(tint, '#ffffff', 0.2), 1], dk: ['#000000', 0.25], lt: ['#ffffff', 0.4], pit: [mix(tint, '#000000', 0.12), 1], idk: ['#000000', 0.3], ilt: ['#ffffff', 0.4], idkG: ['#000000', 0.25], iltG: ['#ffffff', 0.5], g0: ['#ffffff', 1], gm: ['#ffffff', 0.96], g1: ['#ffffff', 0.85], ink: [mix(tint, '#000000', 0.45), 1], led: ['#ffffff', 1], rim: ['#ffffff', 0.95], glow: '#ffffff' },
      TD: { bg: ['#141414', 1], f0: [mix('#141414', tint, 0.28), 1], f1: [mix('#141414', tint, 0.12), 1], dk: ['#000000', 1], lt: [tint, 0.16], pit: ['#0a0a0a', 1], idk: ['#000000', 0.8], ilt: [tint, 0.25], idkG: ['#000000', 0.6], iltG: [tint, 0.4], g0: [mix(tint, '#ffffff', 0.2), 1], gm: [tint, 1], g1: [mix(tint, '#000000', 0.3), 1], ink: [tint, 0.55], led: [tint, 1], rim: [tint, 1], glow: tint },
      // Higher-transparency glass, matched to the Photos icon in the Dock: a near-clear tile and
      // half-transparent white parts that brighten where they overlap.
      GL: { bg: ['#ffffff', 0.1], f0: ['#ffffff', 0.52], f1: ['#ffffff', 0.3], dk: ['#000000', 0.12], lt: ['#ffffff', 0.2], pit: ['#ffffff', 0.08], idk: ['#000000', 0.16], ilt: ['#ffffff', 0.45], idkG: ['#000000', 0.14], iltG: ['#ffffff', 0.5], g0: ['#ffffff', 0.88], gm: ['#ffffff', 0.75], g1: ['#ffffff', 0.58], ink: ['#ffffff', 0.65], led: ['#ffffff', 0.92], rim: ['#ffffff', 0.62], glow: '#ffffff' },
      GD: { bg: ['#000000', 0.16], f0: ['#ffffff', 0.32], f1: ['#ffffff', 0.15], dk: ['#000000', 0.25], lt: ['#ffffff', 0.08], pit: ['#000000', 0.14], idk: ['#000000', 0.3], ilt: ['#ffffff', 0.25], idkG: ['#000000', 0.28], iltG: ['#ffffff', 0.35], g0: ['#ffffff', 0.78], gm: ['#ffffff', 0.62], g1: ['#ffffff', 0.46], ink: ['#ffffff', 0.55], led: ['#ffffff', 0.88], rim: ['#ffffff', 0.45], glow: '#ffffff' },
      // Glass versions of the default, dark and tinted appearances: the ground stays opaque, as Apple
      // asks of a background layer, and the parts above it turn see-through.
      Lg: { bg: ['#d9e1e4', 1], f0: ['#ffffff', 0.58], f1: ['#ffffff', 0.26], dk: ['#788991', 0.45], lt: ['#ffffff', 0.55], pit: ['#788991', 0.16], idk: ['#4f5f66', 0.4], ilt: ['#ffffff', 0.7], idkG: ['#043d1e', 0.45], iltG: ['#c9f7dc', 0.6], g0: ['#3fdc84', 0.85], gm: ['#18b05a', 0.72], g1: ['#0f8a45', 0.6], ink: ['#555555', 0.75], led: ['#18b05a', 0.95], rim: ['#18b05a', 0.78], glow: '#18b05a' },
      Dg: { bg: ['#393939', 1], f0: ['#ffffff', 0.28], f1: ['#ffffff', 0.1], dk: ['#000000', 0.45], lt: ['#ffffff', 0.1], pit: ['#000000', 0.25], idk: ['#000000', 0.45], ilt: ['#ffffff', 0.18], idkG: ['#021f0f', 0.5], iltG: ['#5fe39a', 0.4], g0: ['#3fdc84', 0.85], gm: ['#18b05a', 0.72], g1: ['#0f8a45', 0.6], ink: ['#ffffff', 0.5], led: ['#18b05a', 0.95], rim: ['#18b05a', 0.75], glow: '#18b05a' },
      Xg: { bg: ['#161718', 1], f0: ['#ffffff', 0.2], f1: ['#ffffff', 0.07], dk: ['#000000', 0.55], lt: ['#ffffff', 0.06], pit: ['#000000', 0.3], idk: ['#000000', 0.5], ilt: ['#ffffff', 0.12], idkG: ['#011209', 0.6], iltG: ['#3fdc84', 0.3], g0: ['#2fc070', 0.8], gm: ['#15a052', 0.66], g1: ['#0b6e36', 0.52], ink: ['#ffffff', 0.4], led: ['#15a052', 0.95], rim: ['#15a052', 0.7], glow: '#15a052' },
      TLg: { bg: [tint, 1], f0: ['#ffffff', 0.48], f1: ['#ffffff', 0.22], dk: ['#000000', 0.15], lt: ['#ffffff', 0.35], pit: ['#000000', 0.1], idk: ['#000000', 0.2], ilt: ['#ffffff', 0.4], idkG: ['#000000', 0.18], iltG: ['#ffffff', 0.5], g0: ['#ffffff', 0.92], gm: ['#ffffff', 0.82], g1: ['#ffffff', 0.68], ink: [mix(tint, '#000000', 0.45), 0.8], led: ['#ffffff', 0.95], rim: ['#ffffff', 0.8], glow: '#ffffff' },
      TDg: { bg: ['#141414', 1], f0: [tint, 0.3], f1: [tint, 0.12], dk: ['#000000', 0.55], lt: [tint, 0.1], pit: ['#000000', 0.3], idk: ['#000000', 0.5], ilt: [tint, 0.2], idkG: ['#000000', 0.4], iltG: [tint, 0.35], g0: [mix(tint, '#ffffff', 0.2), 0.9], gm: [tint, 0.78], g1: [mix(tint, '#000000', 0.3), 0.62], ink: [tint, 0.5], led: [tint, 0.95], rim: [tint, 0.8], glow: tint },
    };
    const c = {};
    Object.entries(P[key]).forEach(([k, val]) => {
      if (Array.isArray(val)) { c[k] = val[0]; c[k + 'O'] = val[1]; } else { c[k] = val; }
    });

    // The border variants: edge width and the colour and strength of its light and dark sides.
    const BL = { hi: '#ffffff', hiO: 1, lo: '#7f8f98', loO: 0.95 };
    const BD = { hi: '#76797c', hiO: 1, lo: '#161616', loO: 1 };
    const BG = { hi: '#a5f5c6', lo: '#0a5e2f', o: 1 };
    const V = [
      { w: 8, L: BL, D: BD, G: BG },
      { w: 3, L: BL, D: BD, G: BG },
      { w: 8, k: 0.45, L: { hi: '#ffffff', hiO: 0.45, lo: '#7f8f98', loO: 0.4 }, D: { hi: '#76797c', hiO: 0.45, lo: '#161616', loO: 0.45 }, G: { hi: '#a5f5c6', lo: '#0a5e2f', o: 0.45 } },
      { w: 14, L: { hi: '#ffffff', hiO: 1, lo: '#5f707a', loO: 1 }, D: { hi: '#8a8d90', hiO: 1, lo: '#0a0a0a', loO: 1 }, G: { hi: '#c8fadc', lo: '#06401f', o: 1 } },
      { w: 8, L: { hi: '#8ff0b9', hiO: 1, lo: '#0a5e2f', loO: 0.9 }, D: { hi: '#3fdc84', hiO: 1, lo: '#04331a', loO: 1 }, G: { hi: '#c8fadc', lo: '#04331a', o: 1 } },
      { w: 8, L: BL, D: BD, G: BG, rim: 'accent' },
      { w: 8, L: BL, D: BD, G: BG, rim: { L: ['#000000', 3, 0.5], D: ['#bebebe', 2, 0.5] } },
      { w: 10, L: { hi: '#ffffff', hiO: 1, lo: '#393939', loO: 1 }, D: { hi: '#d9e1e4', hiO: 0.6, lo: '#000000', loO: 1 }, G: { hi: '#ffffff', lo: '#393939', o: 1 } },
      { w: 8, L: BL, D: BD, G: BG, glow: 0.55 },
      { w: 14, soft: true, L: { hi: '#f5f5f6', hiO: 1, lo: '#a7a7a7', loO: 1 }, D: { hi: '#555555', hiO: 1, lo: '#222222', loO: 1 }, G: { hi: '#8ff0b9', lo: '#0f8a45', o: 1 } },
    ];
    const s = V[vi - 1];
    const k = s.k ?? 1;
    const SB = {
      CL: { hi: '#ffffff', hiO: 0.95, lo: '#000000', loO: 0.22 },
      CD: { hi: '#ffffff', hiO: 0.45, lo: '#000000', loO: 0.55 },
      TL: { hi: '#ffffff', hiO: 0.9, lo: '#000000', loO: 0.28 },
      TD: { hi: mix(tint, '#ffffff', 0.2), hiO: 0.6, lo: '#000000', loO: 0.9 },
      GL: { hi: '#ffffff', hiO: 0.85, lo: '#ffffff', loO: 0.22 },
      GD: { hi: '#ffffff', hiO: 0.55, lo: '#ffffff', loO: 0.12 },
      Lg: { hi: '#ffffff', hiO: 0.95, lo: '#ffffff', loO: 0.35 },
      Dg: { hi: '#ffffff', hiO: 0.5, lo: '#ffffff', loO: 0.1 },
      Xg: { hi: '#ffffff', hiO: 0.4, lo: '#ffffff', loO: 0.08 },
      TLg: { hi: '#ffffff', hiO: 0.9, lo: '#ffffff', loO: 0.25 },
      TDg: { hi: mix(tint, '#ffffff', 0.3), hiO: 0.55, lo: '#ffffff', loO: 0.08 },
    };
    const own = key === 'L' ? s.L : (key === 'D' || key === 'X') ? s.D : null;
    const bev = own || { hi: SB[key].hi, hiO: SB[key].hiO * k, lo: SB[key].lo, loO: SB[key].loO * k };
    const gb = own ? { ghi: s.G.hi, ghiO: s.G.o, glo: s.G.lo, gloO: s.G.o } : { ghi: bev.hi, ghiO: bev.hiO, glo: bev.lo, gloO: bev.loO };

    let rim = ['#000000', 0, 0];
    if (s.rim === 'accent') rim = [c.rim, 6, c.rimO];
    else if (s.rim) rim = key === 'L' ? s.rim.L : (key === 'D' || key === 'X') ? s.rim.D : [c.rim, 3, c.rimO * 0.5];

    // Small-size artwork: a thicker edge and rim, lighter shadows and no glows, so the
    // shapes still separate at 32 and 16 px.
    const w = small ? Math.round(s.w * 1.5) : s.w;
    const rw = small ? rim[1] * 2 : rim[1];
    const sh = small ? 0.6 : 1;
    const e = Object.assign({ w, sb: s.soft ? 4 : 0, rim: rim[0], rimW: rw, rimO: rim[2] }, bev, gb, {
      glowO: small ? 0 : (s.glow ?? 0),
      ledGlowO: small ? 0 : 0.7 * c.ledO,
    });

    // Shadow and inner-shadow geometry per design.
    const S = {
      1: { ob: 10, od: 26, ol: -22, io: 16, ion: -12, ib: 5 },
      2: { ob: 8, od: 20, ol: -16, io: 16, ion: -12, ib: 5 },
      3: { ob: 8, od: 20, ol: -16, io: 14, ion: -10, ib: 4 },
      4: { ob: 7, od: 18, ol: -14, io: 12, ion: -10, ib: 4 },
      5: { ob: 16, od: 24, ol: -20, io: 16, ion: -12, ib: 5 },
      6: { ob: 14, od: 22, ol: -18, io: 14, ion: -10, ib: 4 },
    }[n];
    const sv = { ob: S.ob * sh, od: S.od * sh, ol: S.ol * sh, io: S.io, ion: S.ion, ib: S.ib };

    // Edges and rims are stroked bands just outside each shape's outline, so they follow every
    // curve and corner and nothing sits under a see-through face.
    const grow = (x, y, ww, hh, r, d) => ({ x: x - d, y: y - d, w: ww + 2 * d, h: hh + 2 * d, r: r + d });
    const bands = (x, y, ww, hh, r) => ({ bv: grow(x, y, ww, hh, r, w / 2), rm: grow(x, y, ww, hh, r, w + rw / 2) });
    const disc = 112 - w - rw;
    const g = {
      H: bands(210, -60, 360, 120, 60),
      Q: bands(196, 222, 632, 112, 56),
      B1: bands(196, 404, 520, 92, 46),
      B2: bands(196, 556, 632, 92, 46),
      B3: bands(196, 708, 420, 92, 46),
      C1: bands(150, 176, 520, 104, 52),
      C2: bands(150, 322, 520, 104, 52),
      C3: bands(150, 468, 520, 104, 52),
      bandOut: 224 + w / 2,
      bandIn: 128 - w / 2,
      rimOut: 224 + w + rw / 2,
      rimIn: 128 - w - rw / 2,
      well: 128 - w - rw - 4,
      disc,
      discBand: disc + w / 2,
      discRim: disc + w + rw / 2,
      ledW: small ? 20 : 14,
      L: {
        bandOut: 166 + w / 2,
        rimOut: 166 + w + rw / 2,
        bandIn: 90 - w / 2,
        rimIn: 90 - w - rw / 2,
        glass: 90 - w - rw - 4,
        led: 90 - w - rw - 22,
        hsB: 20 + 2 * w,
        hsR: 20 + 2 * (w + rw),
      },
    };

    const frames = {
      L: '1px solid rgba(0, 0, 0, 0.08)', TL: '1px solid rgba(0, 0, 0, 0.08)',
      CL: '1.5px solid rgba(255, 255, 255, 0.55)', CD: '1.5px solid rgba(255, 255, 255, 0.3)',
      D: 'none', X: 'none', TD: 'none',
      GL: '1px solid rgba(0, 0, 0, 0.22)', GD: '1px solid rgba(0, 0, 0, 0.4)',
      Lg: '1px solid rgba(0, 0, 0, 0.1)', TLg: '1px solid rgba(0, 0, 0, 0.1)',
      Dg: '1px solid rgba(0, 0, 0, 0.35)', Xg: '1px solid rgba(0, 0, 0, 0.35)', TDg: '1px solid rgba(0, 0, 0, 0.35)',
    };
    const EDGE = { GL: 0.65, GD: 0.4, Lg: 0.55, Dg: 0.35, Xg: 0.28, TLg: 0.55, TDg: 0.32 };
    const flags = {};
    for (let i = 1; i <= 6; i++) flags['is' + i] = n === i;
    return Object.assign({
      c,
      e,
      s: sv,
      g,
      id: { face: u + 'f', accR: u + 'ar', accL: u + 'al', bvOut: u + 'bo', bvIn: u + 'bi', bvG: u + 'bg', up: u + 'u', inn: u + 'i', innG: u + 'ig', soft: u + 's', glow: u + 'w', edge: u + 'e' },
      px: size + 'px',
      radius: Math.round(size * 0.225) + 'px',
      frame: frames[key],
      tileEdge: EDGE[key] ?? 0,
      label: `${ICONS[n - 1]}, ${NAMES[vi - 1]} border, ${mode.replace('-', ' ')} appearance${glass ? ', glass' : ''}${small ? ', small-size artwork' : ''}`,
    }, flags);
  }
}

// The approved props: Green Rim border, glass, full-size artwork.
function vals(n, appearance) {
  return new Component({ icon: n, variant: 6, appearance, glass: true, size: 1024 }).renderVals();
}

// Design n's markup: keeps its `sc-if` block, drops the others and fills
// every `{{path}}` hole from `v`. An unknown hole is an error.
function markup(n, v) {
  const kept = TEMPLATE.replace(
    /<sc-if value="\{\{is(\d)\}\}"[^>]*>([\s\S]*?)<\/sc-if>/g,
    (_, which, body) => (Number(which) === n ? body : ''),
  );
  return kept.replace(/\{\{\s*([\w.]+)\s*\}\}/g, (_, path) => {
    const value = path.split('.').reduce((o, key) => (o == null ? undefined : o[key]), v);
    if (value === undefined) throw new Error(`design ${n}: no value for {{${path}}}`);
    return String(value);
  });
}

// The foreground of design n in `appearance`: every part, with the ground
// left transparent, on a 1024 x 1024 CSS px page.
export function foreground(n, appearance) {
  const v = vals(n, appearance);
  v.c = { ...v.c, bgO: 0 };
  return [
    '<!doctype html>',
    '<html lang="en"><head><meta charset="utf-8"><title>icon</title>',
    '<style>html,body{margin:0;background:transparent}</style></head>',
    '<body><div style="position: relative; width: 1024px; height: 1024px; line-height: 0">',
    markup(n, v),
    '</div></body></html>',
    '',
  ].join('\n');
}

// The ground colour of design n in `appearance`; it must be opaque.
export function ground(n, appearance) {
  const { c } = vals(n, appearance);
  if (c.bgO !== 1) throw new Error(`design ${n} ${appearance}: the ground is not opaque`);
  return c.bg;
}

const out = process.argv[2];
if (!out) {
  console.error('usage: node scripts/icons/neu-icon.mjs OUT_DIR');
  process.exit(2);
}
mkdirSync(out, { recursive: true });
const fills = [];
for (const { n, slug } of DESIGNS) {
  for (const [appearance, look] of [['default', 'light'], ['dark', 'dark']]) {
    writeFileSync(join(out, `${slug}-${look}.html`), foreground(n, appearance));
    fills.push(`${slug}-${look} ${ground(n, appearance)}`);
  }
}
writeFileSync(join(out, 'fills.txt'), fills.join('\n') + '\n');
