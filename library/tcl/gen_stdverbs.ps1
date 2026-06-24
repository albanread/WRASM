# gen_stdverbs.ps1 — data-driven generator for the nano-TCL standard verb tables.
#
# Source of truth: the DOCUMENTED API in docs/api-guide.md section 14 ("Quick
# reference") — the curated, user-facing verbs, NOT the raw capital/.globl
# linker surface (which also exports threads/DSP/parser internals that are
# unsafe or useless as one-word verbs). The §14 set is encoded in $spec below,
# split CPU vs GPU, and every name is VERIFIED to resolve in its module (a
# proc-form proc or a .globl'd code label) — drift is reported, not emitted.
#
# Emits two backend tables, each with one IFDEF-gated section per subsystem:
#   library/tcl/stdverbs_cpu.was   library/tcl/stdverbs_gpu.was
# stdverbs.was is a shell: `IFDEF NANOTCL_GPU` picks the GPU table, else CPU.
# A game enables a subsystem's verbs by defining its flag before the include:
#   NTV_canvas equ 1   NTV_sprite equ 1   NTV_audio equ 1   ...  (then call TclStdVerbs)
# so it exposes exactly the modules it links. Re-run when §14 or a module changes:
#   powershell library/tcl/gen_stdverbs.ps1

$repo = Split-Path (Split-Path (Split-Path $PSCommandPath))   # ...\RASM
$lib  = Join-Path $repo "library"
$gpu  = Join-Path $repo "gpu"
$snd  = Join-Path $lib  "audio\sound"
$mus  = Join-Path $lib  "audio\music"

# §14 documented API. Each subsystem: a flag, the CPU verb list, the GPU verb
# list, and the module file(s) the names must resolve against (per backend).
# Audio/music are backend-agnostic (shared -> same list/files for both).
function S($p) { @($p -split '\s+' | Where-Object { $_ }) }
$spec = @(
  @{ flag="canvas";     cpu=S("Cls Pset Pget HLine VLine FillRect Rect Line Circle Disc Text SeedLinePalette SetLineColour CyclePalette");
                         gpu=S("Cls Pset Pget HLine VLine FillRect Rect Line Circle Disc Text SeedLinePalette SetLineColour CyclePalette");
                         cmod=@("$lib\canvas.was"); gmod=@("$gpu\canvas.was") }
  @{ flag="sprite";     cpu=S("DefineSprite SpriteColour AddFrame SetFrame DrawSprite");
                         gpu=S("SprAddFrame SprSetPalette SprPush DrawSprites");
                         cmod=@("$lib\sprite.was"); gmod=@("$gpu\sprite.was") }
  @{ flag="tile";       cpu=S("DefineTileset AddTile TilesetColour DrawTile DrawTiles LayerScroll ScrollPresent");
                         gpu=S("TileInit AddTile DrawTiles TileSetScroll");
                         cmod=@("$lib\tile.was"); gmod=@("$gpu\tile.was") }
  @{ flag="blit";       cpu=S("DestFramebuffer SetDest SetKey Blit BlitKey BlitEx");
                         gpu=S("BlitInit PoolAlloc BlitToLayer BlitToBack");
                         cmod=@("$lib\blit.was"); gmod=@("$gpu\blit.was") }
  @{ flag="fx";         cpu=S("SaveBase Fade Flash ColourCycle Mix");
                         gpu=S("FxInit FxRender FxSetTime");
                         cmod=@("$lib\fx.was"); gmod=@("$gpu\fx.was") }
  @{ flag="mode7";      cpu=@(); gpu=S("Mode7Init Mode7RenderLayer Mode7SetCamZ Mode7SetParams Mode7SetWrap");
                         cmod=@(); gmod=@("$gpu\mode7.was") }
  @{ flag="particles";  cpu=@(); gpu=S("PtInit PtSetAtlas PtBurst PtBurstSprite PtUpdate PtDraw PtClear");
                         cmod=@(); gmod=@("$gpu\particles.was") }
  @{ flag="wallfx";     cpu=@(); gpu=S("WallFxInit WallFxRender WallFxSetTime WallComposite");
                         cmod=@(); gmod=@("$gpu\wallfx.was") }
  @{ flag="input";      cpu=S("KeyDown KeyHit MouseX MouseY Action ActionHit SimAction");
                         gpu=S("KeyDown KeyHit MouseX MouseY Action ActionHit SimAction");
                         cmod=@("$lib\input.was"); gmod=@("$gpu\input.was") }
  @{ flag="introspect"; cpu=S("Snapshot"); gpu=S("Snapshot");
                         cmod=@("$lib\introspect.was"); gmod=@("$gpu\introspect.was") }
  @{ flag="audio";      cpu=S("AudioInit Play StopAll AudioShutdown Beep Coin Jump Zap Explode Tone WriteWav");
                         gpu=S("AudioInit Play StopAll AudioShutdown Beep Coin Jump Zap Explode Tone WriteWav");
                         cmod=@(Get-ChildItem "$snd\*.was" | ForEach-Object FullName);
                         gmod=@(Get-ChildItem "$snd\*.was" | ForEach-Object FullName) }
  @{ flag="music";      cpu=S("ParseTune WriteTune MusicInit MusicShutdown");
                         gpu=S("ParseTune WriteTune MusicInit MusicShutdown");
                         cmod=@(Get-ChildItem "$mus\*.was" | ForEach-Object FullName);
                         gmod=@(Get-ChildItem "$mus\*.was" | ForEach-Object FullName) }
)

# Classify every public name a backend's module file(s) define, so we register a
# verb ONLY if it is a real callable proc:
#   "proc" — a `proc Name ... endproc` (macro form, always code)
#   "code" — a `.globl Name` backed by a `Name:` label whose first significant
#            following line is an INSTRUCTION (not a data directive)
# A `.globl`'d DATA symbol (`name QWORD ?`) has no `Name:` label, and a labelled
# data block (`Name:` then `BYTE …`) is rejected by the look-ahead — neither
# becomes a verb. Returns a hashtable name -> "proc"|"code".
function Defined($files) {
  # a DATA directive is the FIRST token on its line (`BYTE "..."`, `QWORD ?`);
  # an instruction's `dword ptr`/`byte ptr` operand keyword is never first (a
  # mnemonic precedes it), so anchoring at the start avoids that false match.
  $dataRe = '^\s*(BYTE|WORD|DWORD|QWORD|REAL4|REAL8|\.byte|\.word|\.long|\.quad|\.ascii|\.asciz)\b'
  $procs=@{}; $globls=@{}; $codeLabels=@{}
  foreach ($f in $files) {
    if (-not (Test-Path $f)) { continue }
    $lines=@(Get-Content $f)
    for ($i=0; $i -lt $lines.Count; $i++) {
      $ln=$lines[$i]
      if ($ln -cmatch '^proc\s+([A-Z][A-Za-z0-9_]*)')   { $procs[$Matches[1]]=$true; continue }
      if ($ln -cmatch '^\.globl\s+([A-Z][A-Za-z0-9_]*)') { $globls[$Matches[1]]=$true; continue }
      if ($ln -cmatch '^([A-Z][A-Za-z0-9_]*):(.*)$') {
        $name=$Matches[1]; $rest=$Matches[2].Trim()
        if ($rest -ne '' -and -not $rest.StartsWith(';')) {
          $isData = ($rest -imatch $dataRe)          # label + decl on one line
        } else {
          $j=$i+1                                     # look at the next significant line
          while ($j -lt $lines.Count -and ($lines[$j].Trim() -eq '' -or $lines[$j].Trim().StartsWith(';'))) { $j++ }
          $isData = ($j -lt $lines.Count) -and ($lines[$j] -imatch $dataRe)
        }
        if (-not $isData) { $codeLabels[$name]=$true }
      }
    }
  }
  $set=@{}
  foreach ($p in $procs.Keys) { $set[$p]="proc" }
  foreach ($g in $globls.Keys) { if ($codeLabels.ContainsKey($g) -and -not $set.ContainsKey($g)) { $set[$g]="code" } }
  $set
}

function GenBackend([string]$backend, [string]$out) {
  $nl="`r`n"; $data=New-Object System.Collections.Generic.List[string]
  $code=New-Object System.Collections.Generic.List[string]; $total=0; $kProc=0; $kCode=0; $warn=@()
  foreach ($e in $spec) {
    $verbs = if ($backend -eq "gpu") { $e.gpu } else { $e.cpu }
    $mods  = if ($backend -eq "gpu") { $e.gmod } else { $e.cmod }
    if (-not $verbs -or $verbs.Count -eq 0) { continue }
    $have = Defined $mods
    $ok = @($verbs | Where-Object { $have.ContainsKey($_) })
    $miss = @($verbs | Where-Object { -not $have.ContainsKey($_) })
    foreach ($m in $miss) { $warn += "  ! $backend/$($e.flag): documented verb '$m' is not a proc in its module - skipped" }
    if ($ok.Count -eq 0) { continue }
    $total += $ok.Count
    $kProc += @($ok | Where-Object { $have[$_] -eq 'proc' }).Count
    $kCode += @($ok | Where-Object { $have[$_] -eq 'code' }).Count
    $data.Add("IFDEF NTV_$($e.flag)")
    foreach ($v in $ok) { $data.Add("svn_$v BYTE `"$v`", 0") }
    $data.Add("ENDIF")
    $code.Add("    IFDEF NTV_$($e.flag)")
    foreach ($v in $ok) {
      $code.Add("    lea     rcx, [rip + svn_$v]")
      $code.Add("    lea     rdx, [rip + $v]")
      $code.Add("    call    TclAddVerb")
    }
    $code.Add("    ENDIF")
  }
  $o=New-Object System.Collections.Generic.List[string]
  $o.Add("; $(Split-Path $out -Leaf) - GENERATED by gen_stdverbs.ps1 from docs/api-guide.md S14.")
  $o.Add("; $($backend.ToUpper()) backend documented API; each subsystem gated by IFDEF NTV_<name>.")
  $o.Add("; Define the NTV_ flags for the modules your game links, then call TclStdVerbs.")
  $o.Add("")
  $o.Add("module Tcl"); $o.Add(""); $o.Add(".DATA"); $o.Add(".balign 8")
  foreach ($l in $data) { $o.Add($l) }
  $o.Add(".balign 16"); $o.Add(""); $o.Add(".CODE"); $o.Add("")
  $o.Add("; TclStdVerbs: register the documented verbs of each ENABLED subsystem")
  $o.Add("proc TclStdVerbs frame")
  foreach ($l in $code) { $o.Add($l) }
  $o.Add("endproc"); $o.Add(""); $o.Add("endmodule")
  ($o -join $nl) | Set-Content -Encoding ASCII $out
  "  $(Split-Path $out -Leaf) : $total verbs ($backend) - $kProc proc-form, $kCode .globl code-label"
  $warn | ForEach-Object { $_ }
}

"generating nano-TCL std verb tables from docs/api-guide.md S14:"
GenBackend "cpu" (Join-Path $lib "tcl\stdverbs_cpu.was")
GenBackend "gpu" (Join-Path $lib "tcl\stdverbs_gpu.was")
