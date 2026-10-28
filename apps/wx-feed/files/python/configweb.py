#!/usr/bin/env python3
"""configweb: the Chumby's "my channels" page, a local, cut-down copy of chumby.com's.

neochumbyd runs it once per server, as the wx-feed app's daemon (neochumby-wx-feed-web).
Phones reach it from the QR code in Settings -> APPS on the Chumby. It edits the config
file that wxfeed.py reads at the start of every render (see config.py):
  - the channel's line-up of apps, in play order (drag and drop, or the arrows)
  - "customize": each app's sub-slides (on/off, order), seconds per slide and its own
    options, as a form built from the app's CONFIG_SCHEMA
  - "remove" takes an app out of the line-up (its settings are kept), "add apps" puts
    it back
  - "SHOW ON CHUMBY NOW" touches refresh-request next to the config, which the renderer
    (the app's companion) takes as "render now"; every save does the same
  - "Chumbys" (/chumbys): every paired Chumby with its shell (home=) and delivery
    (nfs or sync), the shell picker, and the Chumbys asking to pair, to approve or refuse
    (neochumbyd_admin.py is the only code that talks to neochumbyd: its admin socket, or
    its command)

Python stdlib only (plus Pillow, already a renderer dependency, for previews and uploads).
The channel pages need no login: LAN only, by design. The Chumbys page does, because
approving a pairing lets a device onto the server: the server owner's token (or, without
neochumbyd's admin socket, a one-time token from a file) starts a session (see
neochumbyd_admin.py). Every value that
reaches the page is escaped; POSTs must come from this page's own origin, admin POSTs also
carry the session's CSRF token, and the Host header must be a LAN name or address, so
another website can't drive it from a browser on the LAN (CSRF, DNS rebinding).

Usage: configweb.py [--port 8080] [--bind 0.0.0.0] [--config FILE] [--out RENDERS]
                    [--hello "HELLO"]
RENDERS is where each Chumby's renders go, {chumby} standing for its name (the companion's
NEOCHUMBY_WX_OUT); the previews and the status box use the most recent one.
"""
import argparse
import html
import ipaddress
import json
import os
import re
import secrets
import shutil
import sys
import tempfile
import threading
import time
import urllib.parse
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

import config
import neochumbyd_admin as admin

HERE = os.path.dirname(os.path.abspath(__file__))
ASSETS = HERE + "/assets"
STATIC = {                                       # the only files /static/ serves
    "octopus.png": (ASSETS + "/icons/default.png", "image/png"),
    "snpro.otf": (ASSETS + "/fonts/SNPro-Bold.otf", "font/otf"),
}
MAX_BODY = 64 * 1024
MAX_UPLOAD_REQ = 60 * 1024 * 1024        # one upload request; the page's JS sends one file per request
IMAGE_FORMATS = {"JPEG", "MPO", "PNG", "GIF", "WEBP", "BMP", "TIFF", "HEIF", "HEIC", "AVIF"}
ARGS = None


def h(s):
    return html.escape(str(s), quote=True)


# ---------------------------------------------------------------- page
CSS = """
@font-face{font-family:SNPro;src:url(/static/snpro.otf)}
*{box-sizing:border-box}
body{margin:0;background:#fff;color:#555;font:13px/1.45 "Trebuchet MS",Verdana,"DejaVu Sans",sans-serif}
a{color:#1d5fb0;text-decoration:none}a:hover{text-decoration:underline}
.wrap{max-width:980px;margin:0 auto;padding:12px 10px 30px;display:flex;gap:10px;align-items:flex-start}
.main{flex:1;min-width:0}
header{display:flex;align-items:center;justify-content:space-between;height:66px}
.logo{display:flex;align-items:center;gap:4px;color:#2350a8;font:700 38px/1 SNPro,"Arial Rounded MT Bold",sans-serif;letter-spacing:-.5px}
.logo img{height:56px}
.hello{color:#f0601e;font-weight:700;text-transform:uppercase;font-size:12px;letter-spacing:.3px}
nav{display:flex;border:1px dotted #2b5fa8;margin-top:4px}
nav a{flex:1;text-align:center;padding:3px 0;color:#1d4f9a;font-weight:700;font-size:12px;border-left:1px dotted #2b5fa8;text-transform:uppercase}
nav a:first-child{border-left:0}nav a.on{background:#2595d9;color:#fff}nav a:hover{text-decoration:none;background:#e8f2fb}nav a.on:hover{background:#2595d9}
.flash{margin:8px 0 0;padding:4px 7px;color:#fff;font-weight:700;font-size:12px;background:#28a0dd}
.flash.err{background:#d40000}.flash.warn{background:#e98a10}
h1{color:#f0601e;font-size:21px;font-weight:700;margin:16px 0 14px}
h2{color:#f0601e;font-size:15px;font-weight:700;margin:16px 0 6px}
.rule{border-top:1px dashed #ccc;margin:14px 0}
p{margin:6px 0}
.row{display:flex;flex-wrap:wrap;gap:6px 14px;align-items:center}
.lbl{color:#f0601e;font-weight:700}
select,input[type=text],input[type=number]{font:inherit;font-size:12px;padding:2px 4px;border:1px solid #bbb;border-radius:3px;background:#fafafa;max-width:100%}
.btn{font:700 12px "Trebuchet MS",Verdana,sans-serif;color:#1d5fb0;background:#fff;border:2px solid #1d5fb0;border-radius:14px;padding:2px 12px;cursor:pointer;text-transform:uppercase}
.btn:hover{background:#eef5fd}
.cards{display:grid;grid-template-columns:repeat(auto-fill,minmax(160px,1fr));gap:10px;margin-top:12px;padding:0;list-style:none}
.card{background:#efede4;border-radius:7px;box-shadow:0 1px 2px rgba(0,0,0,.18);overflow:hidden;position:relative}
.card.dragging{opacity:.4}.card.over{outline:2px dashed #2595d9}
.card .t{background:#e2e0d5;padding:3px 7px;font-weight:700;font-size:12px;color:#555;white-space:nowrap;overflow:hidden;text-overflow:ellipsis;cursor:grab}
.card .b{display:flex;gap:8px;padding:6px 7px 4px}
.card img{width:80px;height:80px;border:1px solid #c9c6b8;display:block;flex:none}
.card .l{display:flex;flex-direction:column;font-size:12px;line-height:1.25;min-width:0}
.card .l button{all:unset;color:#1d5fb0;cursor:pointer;font-size:12px}.card .l button:hover{text-decoration:underline}
.card .n{color:#888;font-size:11px;margin-top:auto}
.card .over12{color:#d06000;font-size:11px}
.card .mv{display:flex;justify-content:space-between;padding:0 7px 5px}
.card .mv button{all:unset;cursor:pointer;color:#1d5fb0;font-size:12px;padding:0 4px}
.card .mv button:disabled{color:#ccc;cursor:default}
.side{width:232px;flex:none;background:#ebe9df;padding:10px 8px 14px;border-radius:2px}
.big{display:block;width:100%;border:0;border-radius:24px;padding:13px 6px;margin:0 0 10px;color:#fff;font:700 14px "Trebuchet MS",Verdana,sans-serif;text-transform:uppercase;cursor:pointer;text-align:center;text-shadow:0 1px 1px rgba(0,0,0,.25);box-shadow:0 2px 3px rgba(0,0,0,.18)}
.big.blue{background:linear-gradient(#6ab8f7,#2f83e6)}.big.orange{background:linear-gradient(#ffc061,#f29018)}.big.green{background:linear-gradient(#7bedb0,#33cf86)}
.box{border-top:1px solid #d8d5c8;padding:10px 4px 2px;font-size:11px;color:#777}
.box b{display:block;color:#555;font-size:12px;text-transform:uppercase;margin-bottom:3px}
footer{max-width:980px;margin:0 auto;padding:10px;border-top:1px dashed #bbb;display:flex;justify-content:space-between;flex-wrap:wrap;gap:6px;font-size:11px;color:#999}
footer a{font-weight:700;margin-left:14px;font-size:11px}
.modal{display:none;position:fixed;inset:0;background:rgba(80,80,80,.45);z-index:9;overflow:auto;padding:40px 10px}
.modal.open{display:block}
.dlg{background:#fff;max-width:440px;margin:0 auto;padding:14px 18px 18px;position:relative;box-shadow:0 3px 14px rgba(0,0,0,.35)}
.dlg .x{position:absolute;right:10px;top:6px;color:#999;font-size:18px;line-height:1}
.dlg h3{color:#f0601e;font-size:17px;margin:2px 22px 10px 0;font-weight:700}
.dlg .f{margin:8px 0}.dlg .f>span{display:block;color:#777;font-size:12px}
.dlg .help{color:#999;font-size:11px}.dlg .e{color:#d40000;font-size:11px;font-weight:700}
.subs{list-style:none;padding:0;margin:3px 0}
.subs li{display:flex;align-items:center;gap:6px;padding:3px 4px;border:1px solid #e5e2d6;background:#f8f7f2;margin-bottom:3px;border-radius:3px}
.subs li label{flex:1}
.subs button,.items button{font:12px sans-serif;border:1px solid #bbb;background:#fff;border-radius:3px;cursor:pointer;padding:0 6px}
.items{display:flex;flex-wrap:wrap;gap:5px;margin:3px 0}
.items span{display:inline-flex;align-items:center;gap:2px;background:#eef4fb;border:1px solid #c5d6ec;border-radius:12px;padding:1px 3px 1px 6px}
.items input{width:86px;border:0;background:transparent;text-transform:uppercase}
.done{display:block;margin:14px auto 0;font-size:16px;padding:3px 16px}
.prev{display:flex;gap:6px;flex-wrap:wrap;margin-top:12px;border-top:1px dashed #ddd;padding-top:8px}
.prev img{width:96px;height:72px;border:1px solid #ccc}
.nojs .js{display:none}.nojs-only{font-size:11px}html:not(.nojs) .nojs-only{display:none}
.lib{list-style:none;padding:0;margin:4px 0;display:grid;grid-template-columns:repeat(auto-fill,minmax(92px,1fr));gap:6px}
.lib li{background:#f4f2ea;border:1px solid #ddd9cb;border-radius:4px;padding:3px;text-align:center}
.lib li.dragging{opacity:.4}.lib img{width:100%;aspect-ratio:4/3;object-fit:cover;display:block;background:#ccc}
.lib button{font:12px sans-serif;border:1px solid #bbb;background:#fff;border-radius:3px;cursor:pointer;padding:0 5px;margin:2px 1px 0}
.up{margin-top:12px;border-top:1px dashed #ddd;padding-top:8px}.up>span{display:block;color:#777;font-size:12px}
.up input[type=file]{max-width:100%;margin:4px 0}.upst{font-size:11px;color:#1d5fb0}
table.ch{border-collapse:collapse;width:100%;margin:6px 0}table.ch th{text-align:left;color:#f0601e;font-size:12px;padding:4px}
table.ch td{border-top:1px dashed #ccc;padding:6px 4px;vertical-align:middle}table.ch form{display:inline}
.code{font:700 18px/1 monospace;letter-spacing:2px;color:#2350a8}.key{color:#999;font:11px monospace}
@media (max-width:760px){
 .wrap{flex-direction:column}.side{width:100%}
 header{height:auto;flex-wrap:wrap;gap:4px}.logo{font-size:30px}.logo img{height:44px}
 nav a{font-size:11px}
 .cards{grid-template-columns:repeat(auto-fill,minmax(148px,1fr))}
 .modal{padding:10px 6px}
}
"""

JS = r"""
document.documentElement.classList.remove('nojs');
function openDlg(id){document.querySelectorAll('.modal.open').forEach(m=>m.classList.remove('open'));
  var m=document.getElementById('cz-'+id);if(m){m.classList.add('open');history.replaceState(null,'','?customize='+id)}}
function closeDlg(){document.querySelectorAll('.modal.open').forEach(m=>m.classList.remove('open'));history.replaceState(null,'','/')}
document.addEventListener('click',function(e){
  var t=e.target.closest('[data-open]');if(t){e.preventDefault();openDlg(t.dataset.open);return}
  if(e.target.matches('.modal')||e.target.closest('.x')){e.preventDefault();closeDlg();return}
  var mv=e.target.closest('[data-sub]');if(mv){e.preventDefault();var li=mv.closest('li');
    if(mv.dataset.sub<0&&li.previousElementSibling)li.parentNode.insertBefore(li,li.previousElementSibling);
    if(mv.dataset.sub>0&&li.nextElementSibling)li.parentNode.insertBefore(li.nextElementSibling,li);return}
  var del=e.target.closest('[data-del]');if(del){e.preventDefault();del.parentNode.remove();return}
  var add=e.target.closest('[data-add]');if(add){e.preventDefault();var box=document.getElementById(add.dataset.add);
    var s=document.createElement('span');s.innerHTML='<input type="text" maxlength="32" autocapitalize="characters"><button type="button" data-del title="remove">×</button>';
    s.firstChild.name=box.dataset.name;box.appendChild(s);s.firstChild.focus();return}
});
document.addEventListener('click',function(e){var d=e.target.closest('[data-delimg]');if(d){e.preventDefault();d.closest('li').remove()}});
document.querySelectorAll('[data-lib]').forEach(function(ul){var dl=null;
  ul.addEventListener('dragstart',function(e){dl=e.target.closest('li');if(dl)dl.classList.add('dragging')});
  ul.addEventListener('dragend',function(){if(dl)dl.classList.remove('dragging');dl=null});
  ul.addEventListener('dragover',function(e){var t=e.target.closest('li');if(dl&&t&&t!==dl){e.preventDefault();
    var all=[].slice.call(ul.children);ul.insertBefore(dl,all.indexOf(dl)<all.indexOf(t)?t.nextSibling:t)}});});
// "remove" and anything else marked data-confirm asks first
document.addEventListener('submit',function(e){var m=e.target.getAttribute('data-confirm');if(m&&!confirm(m))e.preventDefault()});
// Uploads: start as soon as pictures are picked, shrink each one in the browser first (a phone
// photo is 3-5 MB, the chumby shows 320x240, the server keeps a 640 px preview), and send
// three at a time, one file per request (keeps each request under the server's size cap).
var UP_EDGE=1600;
async function shrink(file){
  try{var bm=await createImageBitmap(file,{imageOrientation:'from-image'});
    var k=Math.min(1,UP_EDGE/Math.max(bm.width,bm.height)),c=document.createElement('canvas');
    c.width=Math.round(bm.width*k);c.height=Math.round(bm.height*k);c.getContext('2d').drawImage(bm,0,0,c.width,c.height);
    var b=await new Promise(function(r){c.toBlob(r,'image/jpeg',0.88)});
    return b&&b.size<file.size?b:file}catch(err){return file}}  // can't decode here (e.g. HEIC on desktop): send as is
async function upload(f){
  var files=[].slice.call(f.files.files);if(!files.length)return;
  var st=f.querySelector('.upst'),bad=[],done=0,next=0;f.querySelector('button').disabled=true;
  st.textContent='Uploading 0 of '+files.length+'...';
  async function worker(){while(next<files.length){var file=files[next++];
    var fd=new FormData();fd.append('app',f.app.value);fd.append('key',f.key.value);fd.append('files',await shrink(file),'photo.jpg');
    try{var r=await fetch('/upload',{method:'POST',body:fd,headers:{'X-Upload':'fetch'}});var j=await r.json();
      if(!j.ok)bad.push(file.name+': '+j.error)}catch(err){bad.push(file.name+': upload failed')}
    st.textContent='Uploading '+(++done)+' of '+files.length+'...'}}
  await Promise.all([worker(),worker(),worker()]);
  st.textContent=bad.length?bad.join('; '):'Done.';
  if(!bad.length||bad.length<files.length)setTimeout(function(){location.href='/?customize='+encodeURIComponent(f.app.value)},bad.length?2500:200)}
document.querySelectorAll('[data-upload]').forEach(function(f){
  f.addEventListener('submit',function(e){e.preventDefault();upload(f)});
  f.files.addEventListener('change',function(){upload(f)})});
document.addEventListener('keydown',function(e){if(e.key==='Escape')closeDlg()});
// drag and drop play order (desktop); the arrows do the same on phones
var drag=null;
document.querySelectorAll('.card[draggable]').forEach(function(c){
  c.addEventListener('dragstart',function(e){drag=c;c.classList.add('dragging');e.dataTransfer.effectAllowed='move';e.dataTransfer.setData('text/plain',c.dataset.app)});
  c.addEventListener('dragend',function(){c.classList.remove('dragging');document.querySelectorAll('.over').forEach(x=>x.classList.remove('over'))});
  c.addEventListener('dragover',function(e){if(drag&&drag!==c){e.preventDefault();c.classList.add('over')}});
  c.addEventListener('dragleave',function(){c.classList.remove('over')});
  c.addEventListener('drop',function(e){e.preventDefault();if(!drag||drag===c)return;
    var all=[].slice.call(c.parentNode.children);
    c.parentNode.insertBefore(drag,all.indexOf(drag)<all.indexOf(c)?c.nextSibling:c);
    var f=document.getElementById('orderform');
    f.order.value=[].map.call(c.parentNode.children,x=>x.dataset.app).join(',');f.submit()});
});
"""


def field_html(app_id, fld, value, err):
    k, t, name = fld["key"], fld["type"], f"opt.{fld['key']}"
    out = [f'<div class="f"><span>{h(fld["label"])}</span>']
    if t == "image_library":
        return library_html(app_id, fld, value, err)
    if t == "list":
        vals = value if isinstance(value, list) else []
        bid = f"items-{h(app_id)}-{h(k)}"
        out.append(f'<div class="items" id="{bid}" data-name="{h(name)}">')
        for v in vals:
            out.append(f'<span><input type="text" name="{h(name)}" value="{h(v)}" maxlength="32" autocapitalize="characters">'
                       f'<button type="button" class="js" data-del title="remove">&times;</button></span>')
        out.append(f'<span><input type="text" name="{h(name)}" value="" maxlength="32" autocapitalize="characters" '
                   f'placeholder="add"></span></div>')
        out.append(f'<button type="button" class="js" data-add="{bid}">+ add another</button>')
    elif t == "bool":
        out.append(f'<input type="hidden" name="bool.{h(k)}" value="1"><label><input type="checkbox" name="{h(name)}" '
                   f'value="1"{" checked" if value else ""}> yes</label>')
    elif t == "choice":
        out.append(f'<select name="{h(name)}">')
        for v, label in fld["choices"]:
            out.append(f'<option value="{h(v)}"{" selected" if v == value else ""}>{h(label)}</option>')
        out.append("</select>")
    elif t == "number":
        attrs = "".join(f' {a}="{h(fld[a])}"' for a in ("min", "max") if fld.get(a) is not None)
        out.append(f'<input type="number" name="{h(name)}" value="{h(value)}"{attrs}'
                   f'{" step=1" if fld.get("integer") else " step=any"} style="width:6em">')
    else:
        out.append(f'<input type="text" name="{h(name)}" value="{h(value)}" maxlength="{h(fld.get("max_len", 200))}" style="width:100%">')
    if fld.get("help"):
        out.append(f'<div class="help">{h(fld["help"])}</div>')
    if err:
        out.append(f'<div class="e">{h(err)}</div>')
    out.append("</div>")
    return "".join(out)


def library_html(app_id, fld, ids, err):
    """Thumbnail grid of an image library. Lives inside the options form: the hidden inputs carry
    the order and DONE saves it. The upload form is separate (forms can't nest): see upload_form."""
    k = fld["key"]
    ids = ids if isinstance(ids, list) else []
    out = [f'<div class="f"><span>{h(fld["label"])} ({len(ids)})</span><ul class="lib" data-lib>']
    for i in ids:
        out.append(f'<li draggable="true"><img src="/library/{h(app_id)}/{h(i)}" alt="" loading="lazy">'
                   f'<input type="hidden" name="opt.{h(k)}" value="{h(i)}">'
                   f'<span class="js"><button type="button" data-sub="-1" title="earlier">&#9664;</button>'
                   f'<button type="button" data-delimg title="delete">&times;</button>'
                   f'<button type="button" data-sub="1" title="later">&#9654;</button></span>'
                   f'<label class="nojs-only"><input type="checkbox" name="del.{h(k)}" value="{h(i)}"> delete</label></li>')
    out.append("</ul>")
    if not ids:
        out.append('<div class="help">No pictures yet: add some below.</div>')
    if fld.get("help"):
        out.append(f'<div class="help">{h(fld["help"])}</div>')
    if err:
        out.append(f'<div class="e">{h(err)}</div>')
    out.append("</div>")
    return "".join(out)


def upload_form(app_id, fld):
    mb = fld.get("max_file_mb", 25)
    return (f'<form method="post" action="/upload" enctype="multipart/form-data" class="up" data-upload>'
            f'<input type="hidden" name="app" value="{h(app_id)}"><input type="hidden" name="key" value="{h(fld["key"])}">'
            f'<span>Add {h(fld["label"].lower())}</span><input type="file" name="files" accept="image/*" multiple>'
            f'<button class="btn">Upload</button><div class="help">Up to {h(mb)} MB each. Save any other changes '
            f'with DONE first: uploading reloads this dialog.</div><div class="upst"></div></form>')


def previews(app_id):
    try:
        with open(os.path.join(render_dir(), "apps.json")) as fh:
            files = json.load(fh).get(app_id) or []
    except (OSError, ValueError):
        return ""
    imgs = "".join(f'<img src="/preview/{h(f)}.png" alt="" loading="lazy">' for f in files[:6] if valid_raw(f))
    return f'<div class="prev">{imgs}</div>' if imgs else ""


def dialog(a, e, is_open, errors=None, form=None):
    """The customize dialog for one app. form: the raw values just posted (shown back with errors)."""
    errors = errors or {}
    aid = a["id"]
    labels = {s["id"]: s["label"] for s in a["subslides"]}
    out = [f'<div class="modal{" open" if is_open else ""}" id="cz-{h(aid)}"><div class="dlg" role="dialog">'
           f'<a class="x" href="/" title="close">&times;</a><h3>customize {h(a["name"].lower())}</h3>'
           f'<form method="post" action="/action"><input type="hidden" name="action" value="save_app">'
           f'<input type="hidden" name="app" value="{h(aid)}">']
    if a["subslides"]:
        out.append('<div class="f"><span>Slides (tick the ones to show; arrows set the order)</span><ul class="subs">')
        for s in e["subslides"]:
            out.append(f'<li><input type="hidden" name="sub" value="{h(s["id"])}"><label><input type="checkbox" name="sub_on" '
                       f'value="{h(s["id"])}"{" checked" if s["enabled"] else ""}> {h(labels.get(s["id"], s["id"]))}</label>'
                       f'<button type="button" class="js" data-sub="-1" title="earlier">&#9650;</button>'
                       f'<button type="button" class="js" data-sub="1" title="later">&#9660;</button></li>')
        out.append("</ul></div>")
    secs = (form or {}).get("seconds", e["seconds"])
    out.append(f'<div class="f"><span>Seconds per slide</span><input type="number" name="seconds" value="{h(secs)}" '
               f'min="{config.SECONDS[0]}" max="{config.SECONDS[1]}" step="1" style="width:6em">'
               + (f'<div class="e">{h(errors["seconds"])}</div>' if "seconds" in errors else "") + "</div>")
    for fld in a["schema"]:
        val = (form or {}).get(fld["key"], e["options"].get(fld["key"]))
        out.append(field_html(aid, fld, val, errors.get(fld["key"])))
    out.append('<button class="btn done">Done</button></form>')
    for fld in a["schema"]:
        if fld["type"] == "image_library":
            out.append(upload_form(aid, fld))
    out.append(previews(aid))
    out.append("</div></div>")
    return "".join(out)


def general_dialog(cfg, is_open, errors=None, form=None):
    errors = errors or {}
    out = [f'<div class="modal{" open" if is_open else ""}" id="cz-general"><div class="dlg">'
           '<a class="x" href="/" title="close">&times;</a><h3>customize the chumby</h3>'
           '<form method="post" action="/action"><input type="hidden" name="action" value="save_general">']
    for fld in config.GENERAL_SCHEMA:
        out.append(field_html("general", fld, (form or {}).get(fld["key"], cfg["general"][fld["key"]]), errors.get(fld["key"])))
    out.append('<button class="btn done">Done</button></form></div></div>')
    return "".join(out)


def render_dir():
    """The render directory to preview: --out is where each Chumby's renders go, with {chumby}
    for its name (a path without it holds one directory per Chumby, or is one render). The
    Chumby rendered most recently wins."""
    out = ARGS.out if "{chumby}" in ARGS.out else os.path.join(ARGS.out, "{chumby}")
    if os.path.isfile(os.path.join(ARGS.out, "manifest.txt")):
        return ARGS.out
    prefix, _, suffix = out.partition("{chumby}")
    best, when = ARGS.out, -1.0
    parent, head = os.path.split(prefix)
    try:
        names = [e[len(head):] for e in os.listdir(parent) if e.startswith(head)]
    except OSError:
        names = []
    for name in names:
        d = prefix + name + suffix
        m = os.path.join(d, "manifest.txt")
        try:
            if admin.NAME.fullmatch(name) and os.path.getmtime(m) > when:
                best, when = d, os.path.getmtime(m)
        except OSError:
            pass
    return best


def status():
    """Last render (the render dir's manifest) and whether a refresh request is pending."""
    st = {"rendered": None, "pages": None}
    try:
        m = os.path.join(render_dir(), "manifest.txt")
        st["rendered"] = os.path.getmtime(m)
        with open(m) as fh:
            st["pages"] = sum(1 for ln in fh if ln.startswith("page "))
    except OSError:
        pass
    try:
        st["refresh_requested"] = os.path.getmtime(refresh_path())
    except OSError:
        st["refresh_requested"] = None
    return st


def ago(ts):
    if not ts:
        return "never"
    s = int(time.time() - ts)
    if s < 90:
        return "just now"
    if s < 5400:
        return f"{s // 60} minutes ago"
    if s < 172800:
        return f"{s // 3600} hours ago"
    return time.strftime("%-d %b %Y", time.localtime(ts))


def top(title, on, flash=None):
    """The page's head, header and nav, and a flash message."""
    links = [("/", "Home", "home"), ("/", "My Chumby", "channels"), ("/chumbys", "Chumbys", "chumbys"), ("/#help", "Help", "help")]
    nav = "".join(f'<a href="{href}"{" class=on" if key == on else ""}>{label}</a>' for href, label, key in links)
    out = [f"""<!doctype html><html lang="en" class="nojs"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1"><title>neochumby - {h(title)}</title>
<link rel="icon" href="/static/octopus.png"><style>{CSS}</style></head><body>
<div class="wrap"><div class="main">
<header><a class="logo" href="/"><img src="/static/octopus.png" alt="">neochumby</a><span class="hello">{h(ARGS.hello)}</span></header>
<nav>{nav}</nav>"""]
    if flash:
        kind, msg = flash
        out.append(f'<div class="flash {h(kind)}">{h(msg)}</div>')
    return "".join(out)


FOOTER = """<footer><span></span><span><a href="/api/config">config file</a><a href="/api/schema">app schemas</a><a href="/api/status">system status</a><br>
a tribute to chumby.com &middot; neochumby</span></footer>"""


def page(cfg, open_id=None, errors=None, form=None, flash=None, code=200):
    reg = {a["id"]: a for a in config.registry()}
    ch = config.channel(cfg)
    kept, dropped = config.plan(cfg)
    cut = {(s["app"]) for s in dropped}
    pages_of = {}
    for s in kept:
        pages_of[s["app"]] = pages_of.get(s["app"], 0) + s["pages"]
    on = [e for e in ch["apps"] if e["enabled"] and e["app"] in reg]
    off = [e for e in ch["apps"] if not e["enabled"] and e["app"] in reg]
    cname = h(ch["title"].lower())
    total = sum(s["pages"] for s in kept)
    out = [top("my channels", "channels", flash)]
    if dropped:
        partly = {s["app"] for s in kept}
        names = [reg[x]["name"] if x not in partly else "part of " + reg[x]["name"]
                 for x in dict.fromkeys(s["app"] for s in dropped)]
        out.append(f'<div class="flash warn">Too many slides: the chumby can only play {config.MAXP} pages, so '
                   f'{h(" and ".join(names))} won\'t show. Untick some slides or remove an app.</div>')
    out.append(f"""<h1>my channels - {cname}</h1>
<p>The channels on a chumby are like channels on a TV: they show a "line-up" of programs (or in this case, apps!).
Each app can show a few slides of its own, which you choose with its <em>customize</em> link. The chumby plays
up to {config.MAXP} slides in all; this channel has {total}.</p>
<div class="rule"></div>
<div class="row"><span>Select a channel to manage:</span><select disabled><option>{h(ch["title"])}</option></select>
<span style="color:#aaa">more channels coming later</span></div>
<h2 id="add">add apps to '{cname}' channel:</h2>""")
    if off:
        opts = "".join(f'<option value="{h(e["app"])}">{h(reg[e["app"]]["name"])}</option>' for e in off)
        out.append(f'<form method="post" action="/action" class="row"><input type="hidden" name="action" value="add">'
                   f'<span class="lbl">apps:</span><select name="app">{opts}</select><button class="btn">Add</button></form>')
        out.append("<p>" + "<br>".join(f'<b>{h(reg[e["app"]]["name"])}</b>: {h(reg[e["app"]]["description"])}' for e in off) + "</p>")
    else:
        out.append("<p>Every app is already in this channel.</p>")
    out.append(f"""<div class="rule"></div><h2>manage apps in '{cname}' channel:</h2>
<p>Drag and drop apps to change play order (or use the arrows). Customize or remove apps using the links beside each
thumbnail.</p>
<form method="post" action="/action" id="orderform"><input type="hidden" name="action" value="order"><input type="hidden" name="order"></form>
<ul class="cards">""")
    for i, e in enumerate(on):
        a = reg[e["app"]]
        n = pages_of.get(a["id"], 0)
        subs = sum(1 for s in e["subslides"] if s["enabled"]) if a["subslides"] else 1
        note = f'{n} page{"s" if n != 1 else ""}' if a["subslides"] or n != 1 else "1 page"
        out.append(f"""<li class="card" draggable="true" data-app="{h(a["id"])}"><div class="t" title="{h(a["name"])}">{h(a["name"])}</div>
<div class="b"><img src="/icon/{h(a["id"])}.png" alt=""><div class="l">
<a href="/?customize={h(a["id"])}#cz-{h(a["id"])}" data-open="{h(a["id"])}">customize</a>
<form method="post" action="/action" data-confirm="Remove {h(a["name"])} from the channel? Its settings are kept if you add it again."><input type="hidden" name="action" value="remove"><input type="hidden" name="app" value="{h(a["id"])}"><button>remove</button></form>
<span class="n">{h(note)}{"" if subs else " (no slides ticked)"}</span>{'<span class="over12">over the limit</span>' if a["id"] in cut else ""}</div></div>
<form method="post" action="/action" class="mv"><input type="hidden" name="action" value="move"><input type="hidden" name="app" value="{h(a["id"])}">
<button name="dir" value="-1" title="play earlier"{" disabled" if i == 0 else ""}>&#9664; earlier</button><button name="dir" value="1" title="play later"{" disabled" if i == len(on) - 1 else ""}>later &#9654;</button></form></li>""")
    out.append("</ul>")
    if not on:
        out.append("<p>No apps in this channel: the chumby falls back to the weather. Add one above.</p>")
    st = status()
    rq = st.get("refresh_requested")
    pending = rq and (not st["rendered"] or rq > st["rendered"])
    out.append(f"""</div><aside class="side">
<form method="post" action="/action"><input type="hidden" name="action" value="refresh"><button class="big blue">Show on chumby now</button></form>
<a class="big orange" href="/?customize=general#cz-general" data-open="general">Chumby settings</a>
<div class="box"><b>Feeder status</b>Last render: {h(ago(st["rendered"]))}{f' ({h(st["pages"])} pages)' if st["pages"] else ""}<br>
{"Refresh requested " + h(ago(rq)) + ", waiting for the renderer." if pending else "The chumby refreshes every 10 minutes, and after you save here."}</div>
<div class="box" id="help"><b>Help</b>Stock symbols are Yahoo Finance's: ASX stocks end in .AX (BHP.AX), US ones are plain (TSLA).
The weather's place is set in the Weather app's customize dialog.
This page is served by your neochumbyd, on your home network only. Pairing new Chumbys, and each one's
shell and delivery, are on the <a href="/chumbys">Chumbys</a> page.</div>
</aside></div>
{FOOTER}""")
    for e in ch["apps"]:
        a = reg.get(e["app"])
        if a:
            mine = open_id == a["id"]
            out.append(dialog(a, e, mine, errors if mine else None, form if mine else None))
    out.append(general_dialog(cfg, open_id == "general", errors if open_id == "general" else None,
                              form if open_id == "general" else None))
    out.append("<script nonce=\"{NONCE}\">" + JS + "</script></body></html>")
    return code, "".join(out)


# ---------------------------------------------------------------- actions
def refresh_path():
    return os.path.join(config.state_dir(ARGS.config), "refresh-request")


def request_refresh():
    try:
        with open(refresh_path(), "w") as fh:
            fh.write(f"{int(time.time())}\n")
        return True
    except OSError as e:
        print(f"configweb: can't write {refresh_path()}: {e}", file=sys.stderr)
        return False


def delete_images(a, ids):
    fn = getattr(a.get("module"), "delete_image", None)
    for i in ids:
        if fn and re.fullmatch(config.IMAGE_ID, i):
            try:
                fn(i)
            except Exception as e:
                print(f"configweb: delete_image({i}): {e}", file=sys.stderr)


def library_field(aid, key):
    """(app, field) for an image_library field, or (None, None)."""
    a = config.app_info(aid)
    if not a or not a.get("module"):
        return None, None
    fld = next((f for f in a["schema"] if f["key"] == key and f["type"] == "image_library"), None)
    return (a, fld) if fld else (None, None)


def dir_size(path):
    total = 0
    for root, _, files in os.walk(path):
        for n in files:
            try:
                total += os.path.getsize(os.path.join(root, n))
            except OSError:
                pass
    return total


def parse_multipart(ctype, body, tmpdir):
    """Minimal multipart/form-data parser (the stdlib's cgi module is gone in 3.13).
    Returns (fields {name: value}, files [temp path]). File parts go straight to temp files
    with random names: the client's filename is never looked at."""
    m = re.search(r'boundary="?([^";]{1,200})"?', ctype or "")
    if not m:
        raise ValueError("not multipart")
    sep = b"--" + m.group(1).encode("latin-1")
    fields, files = {}, []
    for part in body.split(sep)[1:]:
        if part[:2] == b"--":
            break
        head, _, data = part.partition(b"\r\n\r\n")
        if data.endswith(b"\r\n"):
            data = data[:-2]
        disp = re.search(rb'content-disposition:[^\r\n]*', head, re.I)
        if not disp:
            continue
        name = re.search(rb'\bname="([^"]*)"', disp.group(0))
        name = name.group(1).decode("utf-8", "replace") if name else ""
        if re.search(rb'\bfilename="', disp.group(0)):
            if not data:
                continue                                    # an empty file input
            path = os.path.join(tmpdir, secrets.token_hex(12))
            with open(path, "wb") as fh:
                fh.write(data)
            files.append((name, path, len(data)))
        else:
            fields[name] = data.decode("utf-8", "replace")[:200]
    return fields, files


def decode_ok(path):
    """True if Pillow really decodes it as an allowed image (never trust the extension)."""
    import warnings
    from PIL import Image
    try:
        import pillow_heif                                   # optional: iPhone HEIC
        pillow_heif.register_heif_opener()
    except Exception:
        pass
    Image.MAX_IMAGE_PIXELS = 80_000_000                       # ~ a 100 MP phone shot is the most we take
    try:
        with warnings.catch_warnings():
            warnings.simplefilter("error", Image.DecompressionBombWarning)
            with Image.open(path) as im:
                if im.format not in IMAGE_FORMATS:
                    return False
                im.verify()
            with Image.open(path) as im:                     # verify() doesn't decode pixels: load them too
                im.load()
        return True
    except Exception:
        return False


def handle_upload(fields, files):
    """Validate and ingest uploaded images. Returns (added ids, [errors])."""
    a, fld = library_field(fields.get("app", ""), fields.get("key", ""))
    if not a:
        return [], ["unknown app or field"]
    mod = a["module"]
    per_file = fld.get("max_file_mb", 25) * 1024 * 1024
    total_cap = fld.get("max_total_mb", 500) * 1024 * 1024
    added, errors = [], []
    with SAVE_LOCK:
        cfg = config.load(ARGS.config)
        entry = next(e for e in config.channel(cfg)["apps"] if e["app"] == a["id"])
        lib = list(entry["options"].get(fld["key"], []))
        try:
            used = dir_size(mod.storage_dir())
        except Exception:
            used = 0
        for n, (_, path, size) in enumerate(files, 1):
            label = f"picture {n}"
            if size > per_file:
                errors.append(f"{label} is over {fld.get('max_file_mb', 25)} MB")
            elif used + size > total_cap:
                errors.append(f"{label}: the library is full ({fld.get('max_total_mb', 500)} MB)")
            elif len(lib) >= fld.get("max_items", 200):
                errors.append(f"{label}: at most {fld.get('max_items', 200)} pictures")
            elif not decode_ok(path):
                errors.append(f"{label} isn't a picture this server can read")
            else:
                try:
                    i = mod.ingest_image(path)
                    if not (isinstance(i, str) and re.fullmatch(config.IMAGE_ID, i)):
                        raise ValueError("bad id from ingest_image")
                    lib.append(i)
                    added.append(i)
                    used = dir_size(mod.storage_dir())
                except ValueError as e:
                    errors.append(f"{label}: {e}")
                except Exception as e:
                    print(f"configweb: ingest_image: {e!r}", file=sys.stderr)
                    errors.append(f"{label} couldn't be processed")
        if added:
            entry["options"][fld["key"]] = lib
            config.save(cfg, ARGS.config)
    return added, errors


def form_to_options(schema, pairs):
    """Form pairs -> raw option dict for config.validate."""
    raw = {}
    multi = {}
    for k, v in pairs:
        if k.startswith("opt."):
            multi.setdefault(k[4:], []).append(v)
    markers = {k[5:] for k, _ in pairs if k.startswith("bool.")}
    for fld in schema:
        k = fld["key"]
        vals = multi.get(k)
        if fld["type"] == "list":
            if vals is not None:
                raw[k] = [v for v in vals if v.strip()]
        elif fld["type"] == "bool":
            if k in markers:
                raw[k] = bool(vals)
        elif vals:
            raw[k] = vals[0]
    return raw


DONE = {   # ?done=KEY after a successful POST: fixed messages, never text from the request
    "remove": "Removed {app}. Its settings are kept if you add it again.",
    "add": "Added {app}.", "move": "Play order changed.", "order": "Play order changed.",
    "save_app": "Saved {app}.", "save_general": "Saved. The QR code on the chumby changes at its next refresh.",
    "refresh": "Asked the feeder to refresh the chumby now.",
    "upload": "Pictures added to {app}.",
}
SAVE_LOCK = threading.Lock()


def done_flash(q):
    key = (q.get("done") or [""])[0]
    if key not in DONE:
        return None
    a = config.app_info((q.get("app") or [""])[0])
    msg = DONE[key].format(app=a["name"] if a else "the app")
    if key not in ("refresh",):
        msg += " The chumby updates at its next refresh."
    return ("ok", msg)


def act(pairs):
    """Apply one POSTed action. Returns (cfg, open_id, errors, form, flash); flash is None on success."""
    f = {}
    for k, v in pairs:
        f.setdefault(k, v)
    cfg = config.load(ARGS.config)
    ch = config.channel(cfg)
    apps = ch["apps"]
    reg = {a["id"]: a for a in config.registry()}
    action, aid = f.get("action"), f.get("app")
    entry = next((e for e in apps if e["app"] == aid), None)
    if action in ("remove", "add", "move", "save_app") and not entry:
        return cfg, None, None, None, ("err", "Unknown app.")
    if action == "remove":
        entry["enabled"] = False
    elif action == "add":
        entry["enabled"] = True
        apps.remove(entry)                                      # added apps go to the end, like chumby.com
        last = max([i for i, e in enumerate(apps) if e["enabled"]], default=-1)
        apps.insert(last + 1, entry)
    elif action == "move":
        on = [e for e in apps if e["enabled"]]
        i, d = on.index(entry), (1 if f.get("dir") == "1" else -1)
        if 0 <= i + d < len(on):
            on[i], on[i + d] = on[i + d], on[i]
        ch["apps"] = on + [e for e in apps if not e["enabled"]]
    elif action == "order":
        want = [x for x in str(f.get("order", "")).split(",") if x in reg]
        on = [e for e in apps if e["enabled"]]
        rank = {x: i for i, x in enumerate(want)}
        on.sort(key=lambda e: rank.get(e["app"], len(want)))
        ch["apps"] = on + [e for e in apps if not e["enabled"]]
    elif action == "save_app":
        a = reg[aid]
        errors, form = {}, {}
        opts_raw = form_to_options(a["schema"], pairs)
        dropped_imgs = {}
        for fld in a["schema"]:                                 # libraries: reorder / drop known ids only
            if fld["type"] == "image_library":
                k, old = fld["key"], entry["options"].get(fld["key"], [])
                gone = {v for kk, v in pairs if kk == f"del.{k}"}
                posted = [v for kk, v in pairs if kk == f"opt.{k}"]
                new = [v for v in dict.fromkeys(posted) if v in old and v not in gone]
                opts_raw[k] = new
                dropped_imgs[k] = [v for v in old if v not in new]
        opts, errs = config.validate(a["schema"], opts_raw)
        errors.update(errs)
        secs, serr = config.validate_seconds(f.get("seconds", entry["seconds"]), entry["seconds"])
        if serr:
            errors["seconds"] = serr
        if errors:
            form = dict(opts_raw, seconds=f.get("seconds", ""))
            return cfg, aid, errors, form, ("err", "Please fix the highlighted fields.")
        order = [v for k, v in pairs if k == "sub"]
        ticked = {v for k, v in pairs if k == "sub_on"}
        if a["subslides"]:
            entry["subslides"] = config.normalise_subslides(a, [{"id": s, "enabled": s in ticked} for s in order])
        entry["options"], entry["seconds"] = opts, secs
        cfg = config.save(cfg, ARGS.config)
        delete_images(a, [i for ids in dropped_imgs.values() for i in ids])
        return cfg, None, None, None, None
    elif action == "save_general":
        raw = form_to_options(config.GENERAL_SCHEMA, pairs)
        gen, errs = config.validate(config.GENERAL_SCHEMA, raw)
        if errs:
            return cfg, "general", errs, raw, ("err", "Please fix the highlighted fields.")
        cfg["general"] = gen
    elif action == "refresh":
        if not request_refresh():
            return cfg, None, None, None, ("err", "Couldn't ask the feeder to refresh (see its log).")
        return cfg, None, None, None, None
    else:
        return cfg, None, None, None, ("err", "Unknown action.")
    cfg = config.save(cfg, ARGS.config)
    return cfg, None, None, None, None


# ---------------------------------------------------------------- previews and icons
def valid_raw(name):
    return isinstance(name, str) and re.fullmatch(r"[a-z0-9_]{1,40}\.raw", name) is not None


def raw_png(name):
    """PNG bytes for a 320x240 page in the render dir: its .png if wxfeed saved one, else
    converted from the RGB565 .raw."""
    import io
    from PIL import Image
    base = os.path.join(render_dir(), name[:-4])
    if os.path.exists(base + ".png"):
        with open(base + ".png", "rb") as fh:
            return fh.read()
    with open(base + ".raw", "rb") as fh:
        raw = fh.read(320 * 240 * 2)
    if len(raw) != 320 * 240 * 2:
        raise OSError("short raw file")
    im = Image.frombytes("RGB", (320, 240), raw, "raw", "BGR;16")
    buf = io.BytesIO()
    im.save(buf, "PNG")
    return buf.getvalue()


# ---------------------------------------------------------------- the Chumbys page (admin)
SESSION_TTL = 12 * 3600
SESSIONS = {}                                    # session id -> (expiry, csrf token, owner token)
FAILS = []                                       # times of recent failed logins
SESS_LOCK = threading.Lock()
COOKIE = "wxadmin"
ADMIN_DONE = {   # ?done=KEY after an admin POST: fixed messages
    "approve": "Paired. The Chumby finishes pairing on its own screen.", "refuse": "Request refused.",
    "forget": "Forgotten. That Chumby must pair again to use this server.",
    "set": "Saved. The Chumby takes its new settings when it next connects.",
    "login": "Signed in.", "logout": "Signed out.",
}


def session_of(headers):
    """(session id, csrf token, owner token) for a request's admin cookie, or None."""
    m = re.search(rf"(?:^|;)\s*{COOKIE}=([A-Za-z0-9_-]{{20,64}})", headers.get("Cookie") or "")
    if not m:
        return None
    with SESS_LOCK:
        s = SESSIONS.get(m.group(1))
        if not s or s[0] < time.time():
            SESSIONS.pop(m.group(1), None)
            return None
        return m.group(1), s[1], s[2]


def log_in(token):
    """(session id, None) for a token that signs in (neochumbyd_admin.login), else (None, why).
    Five failures in a minute lock logins for the rest of that minute; every failure costs a
    second. The owner token stays in memory with the session, for approving pairings."""
    now = time.time()
    with SESS_LOCK:
        FAILS[:] = [t for t in FAILS if now - t < 60]
        if len(FAILS) >= 5:
            return None, "Too many tries: wait a minute."
    try:
        ok = admin.login(token)
    except admin.AdminError:
        ok = False
    if not ok:
        with SESS_LOCK:
            FAILS.append(now)
        time.sleep(1)
        return None, "That token doesn't sign in."
    sid = secrets.token_urlsafe(24)
    with SESS_LOCK:
        for k in [k for k, s in SESSIONS.items() if s[0] < now]:
            del SESSIONS[k]
        SESSIONS[sid] = (now + SESSION_TTL, secrets.token_urlsafe(18), token.strip() if admin.uses_socket() else None)
    return sid, None


def login_page(flash=None, code=200):
    what, once = admin.login_help()
    return code, (top("sign in", "chumbys", flash) + f"""<h1>sign in to manage chumbys</h1>
<p>Pairing a Chumby lets it use this server, so this page needs a token: {h(what)}.
{"It works once: signing in writes a new one." if once else ""}</p>
<form method="post" action="/login" class="row"><span class="lbl">admin token:</span>
<input type="password" name="token" autocomplete="off" maxlength="64" style="width:18em"><button class="btn">Sign in</button></form>
</div></div>{FOOTER}</body></html>""")


def chumbys_page(sess, flash=None, code=200):
    csrf = f'<input type="hidden" name="csrf" value="{h(sess[1])}">'
    try:
        chumbys, pending = admin.listing()
    except admin.AdminError as e:
        chumbys, pending, flash = [], [], flash or ("err", f"Couldn't ask neochumbyd: {e}")
    shells = admin.shells()
    out = [top("chumbys", "chumbys", flash), """<h1>chumbys</h1>
<p>Every Chumby paired with this server. Its <b>shell</b> is the home screen it starts in. Its <b>delivery</b>
is how it gets its apps and pictures: read over the network as it needs them (NFS), or copied onto its USB
stick (sync).</p>"""]
    out.append("<h2>asking to pair</h2>")
    if pending:
        out.append('<table class="ch"><tr><th>name</th><th>code</th><th>from</th><th></th></tr>')
        for p in pending:
            out.append(f"""<tr><td>{h(p.name)}</td><td class="code">{h(p.code)}</td><td>{h(p.addr)}</td><td>
<form method="post" action="/admin" data-confirm="Pair {h(p.name)}? Only if the Chumby shows {h(p.code)}.">{csrf}
<input type="hidden" name="action" value="approve"><input type="hidden" name="code" value="{h(p.code)}"><button class="btn">Approve</button></form>
<form method="post" action="/admin">{csrf}<input type="hidden" name="action" value="refuse"><input type="hidden" name="code" value="{h(p.code)}">
<button class="btn">Refuse</button></form></td></tr>""")
        out.append("</table><p>Approve a request only if its code is the one on the Chumby's own screen.</p>")
    else:
        out.append("<p>None. Tap PAIR on a Chumby's home screen and its request shows here.</p>")
    out.append("<h2>paired</h2>")
    if chumbys:
        out.append('<table class="ch"><tr><th>name</th><th>shell</th><th>delivery</th><th></th></tr>')
        for c in chumbys:
            home, how = c.settings.get(admin.SHELL_SETTING, ""), c.settings.get("delivery", "")
            opts = [("", "neochumby-home (default)")] + shells
            if home and home not in [a for a, _ in shells]:
                opts.append((home, f"{home} (not in this tree)"))
            sel = "".join(f'<option value="{h(a)}"{" selected" if a == home else ""}>{h(t)}</option>' for a, t in opts)
            dsel = "".join(f'<option value="{v}"{" selected" if v == how else ""}>{label}</option>'
                           for v, label in (("", "default"), ("nfs", "network (NFS)"), ("sync", "USB stick (sync)")))
            out.append(f"""<tr><td>{h(c.name)}<br><span class="key">{h(c.key[:16])}</span></td>
<td colspan="2"><form method="post" action="/admin">{csrf}<input type="hidden" name="action" value="set">
<input type="hidden" name="name" value="{h(c.name)}"><select name="shell">{sel}</select> <select name="delivery">{dsel}</select>
<button class="btn">Save</button></form></td><td>
<form method="post" action="/admin" data-confirm="Forget {h(c.name)}? It will have to pair again.">{csrf}
<input type="hidden" name="action" value="forget"><input type="hidden" name="name" value="{h(c.name)}"><button class="btn">Forget</button></form></td></tr>""")
        out.append("</table>")
    else:
        out.append("<p>No Chumby is paired with this server yet.</p>")
    out.append(f"""<div class="rule"></div><form method="post" action="/logout">{csrf}<button class="btn">Sign out</button></form>
</div></div>{FOOTER}<script nonce="{{NONCE}}">document.addEventListener('submit',function(e){{var m=e.target.getAttribute('data-confirm');
if(m&&!confirm(m))e.preventDefault()}})</script></body></html>""")
    return code, "".join(out)


def admin_act(f, sess):
    """Apply one admin POST. Returns (done key, None) or (None, error message)."""
    action = f.get("action")
    try:
        if action == "approve":
            admin.approve(f.get("code", ""), sess[2])
        elif action == "refuse":
            admin.refuse(f.get("code", ""))
        elif action == "forget":
            admin.forget(f.get("name", ""))
        elif action == "set":
            admin.set_shell(f.get("name", ""), f.get("shell", ""))
            admin.set_delivery(f.get("name", ""), f.get("delivery", ""))
        else:
            return None, "Unknown action."
    except admin.AdminError as e:
        return None, str(e)
    return action, None


# ---------------------------------------------------------------- HTTP
def host_ok(host):
    """LAN names and addresses only: stops DNS rebinding from a website on the internet."""
    host = (host or "").strip().lower()
    m = re.fullmatch(r"(?:\[([0-9a-f:.]+)\]|([a-z0-9.\-]+))(?::\d{1,5})?", host)
    if not m:
        return False
    name = m.group(1) or m.group(2)
    try:
        ip = ipaddress.ip_address(name)
        return ip.is_private or ip.is_loopback or ip.is_link_local
    except ValueError:
        pass
    allowed = {"localhost", urllib.parse.urlsplit(config.load(ARGS.config)["general"]["config_url"]).hostname or ""}
    return name in allowed or "." not in name or name.endswith((".localdomain", ".local", ".lan", ".home.arpa", ".localhost"))


class Handler(BaseHTTPRequestHandler):
    server_version = "chumby-configweb"

    def log_message(self, fmt, *a):
        sys.stderr.write("%s %s\n" % (self.address_string(), fmt % a))

    def send(self, code, body, ctype="text/html; charset=utf-8", extra=None):
        nonce = secrets.token_urlsafe(16)
        if isinstance(body, str):
            body = body.replace('nonce="{NONCE}"', f'nonce="{nonce}"').encode()
        self.send_response(code)
        self.send_header("Content-Type", ctype)
        self.send_header("Content-Length", str(len(body)))
        self.send_header("X-Content-Type-Options", "nosniff")
        self.send_header("X-Frame-Options", "DENY")
        self.send_header("Referrer-Policy", "same-origin")
        self.send_header("Content-Security-Policy", f"default-src 'self'; img-src 'self'; style-src 'unsafe-inline'; "
                         f"script-src 'nonce-{nonce}'; form-action 'self'; frame-ancestors 'none'; base-uri 'none'")
        if ctype.startswith(("text/html", "application/json")):
            self.send_header("Cache-Control", "no-store")
        for k, v in (extra or {}).items():
            self.send_header(k, v)
        self.end_headers()
        if self.command != "HEAD":
            self.wfile.write(body)

    def file(self, path, ctype):
        try:
            with open(path, "rb") as fh:
                data = fh.read()
        except OSError:
            return self.send(404, "not found", "text/plain")
        self.send(200, data, ctype, {"Cache-Control": "max-age=300"})

    def upload(self):
        fetch = self.headers.get("X-Upload") == "fetch"
        def reply(ok, aid, err=""):
            if fetch:
                return self.send(200 if ok else 400, json.dumps({"ok": ok, "error": err}), "application/json")
            if ok:
                return self.send(303, "", "text/plain", {"Location": "/?" + urllib.parse.urlencode(
                    {"customize": aid, "done": "upload", "app": aid})})
            code, body = page(config.load(ARGS.config), open_id=aid if config.app_info(aid) else None,
                              flash=("err", err), code=400)
            return self.send(code, body)
        try:
            n = int(self.headers.get("Content-Length") or -1)
        except ValueError:
            n = -1
        if not 0 < n <= MAX_UPLOAD_REQ:
            # drain nothing: the connection closes after the reply
            self.close_connection = True
            return reply(False, "", f"Too big: send at most {MAX_UPLOAD_REQ // 1048576} MB at a time.")
        tmpdir = tempfile.mkdtemp(prefix="chumby-upload-")
        try:
            body = self.rfile.read(n)
            try:
                fields, files = parse_multipart(self.headers.get("Content-Type"), body, tmpdir)
            except ValueError:
                return reply(False, "", "Not an upload.")
            del body
            aid = fields.get("app", "")
            if not files:
                return reply(False, aid, "Choose some pictures first.")
            added, errors = handle_upload(fields, files)
            if errors and not added:
                return reply(False, aid, "; ".join(errors))
            if errors:
                return reply(False, aid, f"Added {len(added)}; " + "; ".join(errors))
            return reply(True, aid)
        finally:
            shutil.rmtree(tmpdir, ignore_errors=True)

    def library_thumb(self, aid, iid):
        """A library image's preview: the module names the file, and it must resolve inside the
        module's storage_dir() (symlinks resolved), so no id can reach anything else."""
        a = config.app_info(aid)
        mod = a.get("module") if a else None
        try:
            base = os.path.realpath(mod.storage_dir())
            path = os.path.realpath(mod.thumb_path(iid))
        except Exception:
            return self.send(404, "not found", "text/plain")
        ext = os.path.splitext(path)[1].lower()
        ctype = {".jpg": "image/jpeg", ".jpeg": "image/jpeg", ".png": "image/png", ".webp": "image/webp"}.get(ext)
        if not ctype or not path.startswith(base + os.sep) or not os.path.isfile(path):
            return self.send(404, "not found", "text/plain")
        return self.file(path, ctype)

    def do_HEAD(self):
        self.do_GET()

    def do_GET(self):
        if not host_ok(self.headers.get("Host")):
            return self.send(421, "unknown host", "text/plain")
        u = urllib.parse.urlsplit(self.path)
        q = urllib.parse.parse_qs(u.query)
        p = u.path
        if p == "/":
            cfg = config.load(ARGS.config)
            want = (q.get("customize") or [None])[0]
            ok = want == "general" or any(e["app"] == want for e in config.channel(cfg)["apps"])
            code, body = page(cfg, open_id=want if ok else None, flash=done_flash(q))
            return self.send(code, body)
        if p.startswith("/static/") and p[8:] in STATIC:
            return self.file(*STATIC[p[8:]])
        m = re.fullmatch(r"/icon/([a-z0-9_]{1,24})\.png", p)
        if m:
            a = config.app_info(m.group(1))
            icon = os.path.join(config.ICON_DIR, a["icon"]) if a and re.fullmatch(r"[a-z0-9_\-]+\.png", a.get("icon", "")) else ""
            return self.file(icon if os.path.exists(icon) else ASSETS + "/chumby-octopus.png", "image/png")
        m = re.fullmatch(r"/library/([a-z0-9_]{1,24})/([A-Za-z0-9_-]{1,64})", p)
        if m:
            return self.library_thumb(m.group(1), m.group(2))
        m = re.fullmatch(r"/preview/([a-z0-9_]{1,40}\.raw)\.png", p)
        if m:
            try:
                return self.send(200, raw_png(m.group(1)), "image/png", {"Cache-Control": "no-cache"})
            except OSError:
                return self.send(404, "not found", "text/plain")
        if p == "/chumbys":
            sess = session_of(self.headers)
            if not sess:
                return self.send(303, "", "text/plain", {"Location": "/login"})
            key = (q.get("done") or [""])[0]
            code, body = chumbys_page(sess, ("ok", ADMIN_DONE[key]) if key in ADMIN_DONE else None)
            return self.send(code, body)
        if p == "/login":
            code, body = login_page()
            return self.send(code, body)
        if p == "/api/config":
            return self.send(200, json.dumps(config.load(ARGS.config), indent=2), "application/json")
        if p == "/api/schema":
            reg = [{k: v for k, v in a.items() if k != "module"} for a in config.registry()]
            return self.send(200, json.dumps({"apps": reg, "general": config.GENERAL_SCHEMA, "maxp": config.MAXP}, indent=2),
                             "application/json")
        if p == "/api/status":
            cfg = config.load(ARGS.config)
            kept, dropped = config.plan(cfg)
            return self.send(200, json.dumps(dict(status(), plan=kept, dropped=dropped), indent=2, default=str), "application/json")
        self.send(404, "not found", "text/plain")

    def do_POST(self):
        if not host_ok(self.headers.get("Host")):
            return self.send(421, "unknown host", "text/plain")
        # same-origin only: a browser always sends Origin (or at least Referer) on a cross-site form POST
        origin = self.headers.get("Origin") or self.headers.get("Referer") or ""
        if origin and urllib.parse.urlsplit(origin).netloc.lower() != (self.headers.get("Host") or "").lower():
            return self.send(403, "cross-origin request refused", "text/plain")
        path = urllib.parse.urlsplit(self.path).path
        if path == "/upload":
            return self.upload()
        if path not in ("/action", "/login", "/logout", "/admin"):
            return self.send(404, "not found", "text/plain")
        try:
            n = int(self.headers.get("Content-Length") or 0)
        except ValueError:
            n = -1
        if not 0 <= n <= MAX_BODY:
            return self.send(413, "too big", "text/plain")
        body = self.rfile.read(n).decode("utf-8", "replace")
        pairs = urllib.parse.parse_qsl(body, keep_blank_values=True, max_num_fields=500)
        if path != "/action":
            return self.admin_post(path, dict(pairs))
        try:
            with SAVE_LOCK:                                    # load-modify-save must not interleave
                cfg, open_id, errors, form, flash = act(pairs)
        except OSError as e:
            print(f"configweb: save failed: {e}", file=sys.stderr)
            code, html_ = page(config.load(ARGS.config), flash=("err", "Couldn't save the settings file."), code=500)
            return self.send(code, html_)
        if flash:                                              # an error: show the page again, with it
            code, html_ = page(cfg, open_id=open_id, errors=errors, form=form, flash=flash, code=400)
            return self.send(code, html_)
        f = dict(pairs)                                        # post/redirect/get
        q = {"done": f.get("action", "")}
        if f.get("app"):
            q["app"] = f["app"]
        self.send(303, "", "text/plain", {"Location": "/?" + urllib.parse.urlencode(q)})

    def admin_post(self, path, f):
        """/login, /logout and /admin. Past login, every POST needs the session and its CSRF token."""
        if path == "/login":
            sid, err = log_in(f.get("token", ""))
            if not sid:
                code, body = login_page(("err", err), 403)
                return self.send(code, body)
            cookie = f"{COOKIE}={sid}; HttpOnly; SameSite=Strict; Path=/; Max-Age={SESSION_TTL}"
            return self.send(303, "", "text/plain", {"Location": "/chumbys?done=login", "Set-Cookie": cookie})
        sess = session_of(self.headers)
        if not sess:
            return self.send(303, "", "text/plain", {"Location": "/login"})
        if not secrets.compare_digest(f.get("csrf", ""), sess[1]):
            return self.send(403, "stale form: reload the page", "text/plain")
        if path == "/logout":
            with SESS_LOCK:
                SESSIONS.pop(sess[0], None)
            cookie = f"{COOKIE}=; HttpOnly; SameSite=Strict; Path=/; Max-Age=0"
            return self.send(303, "", "text/plain", {"Location": "/login", "Set-Cookie": cookie})
        done, err = admin_act(f, sess)
        if err:
            code, body = chumbys_page(sess, ("err", err), 400)
            return self.send(code, body)
        self.send(303, "", "text/plain", {"Location": "/chumbys?done=" + done})


def main(argv=None):
    global ARGS
    ap = argparse.ArgumentParser(description="The Chumby's my-channels config page")
    ap.add_argument("--port", type=int, default=8080)
    ap.add_argument("--bind", default="0.0.0.0")
    ap.add_argument("--config", default=None, help="config file (default: see config.default_path)")
    ap.add_argument("--out", default=None, help="where each Chumby's renders go, {chumby} for its name (default: neochumbyd's overlay)")
    ap.add_argument("--hello", default="")
    ARGS = ap.parse_args(argv)
    ARGS.config = ARGS.config or config.default_path()
    ARGS.out = ARGS.out or os.path.join(admin.neochumbyd_state(), "overlay", "{chumby}", "apps", "wx-feed", "out")
    srv = ThreadingHTTPServer((ARGS.bind, ARGS.port), Handler)
    print(f"configweb: http://{ARGS.bind}:{ARGS.port}/  config {ARGS.config}  renders {ARGS.out}", file=sys.stderr)
    if admin.uses_socket():
        print("configweb: the Chumbys page signs in with the owner token (`neochumbyd token`)", file=sys.stderr)
    else:
        try:
            print(f"configweb: the one-time admin token for the Chumbys page is in {admin.ensure_token()}", file=sys.stderr)
        except OSError as e:
            print(f"configweb: can't write the admin token ({e}): the Chumbys page can't be signed in to", file=sys.stderr)
    try:
        srv.serve_forever()
    except KeyboardInterrupt:
        pass


if __name__ == "__main__":
    main()
