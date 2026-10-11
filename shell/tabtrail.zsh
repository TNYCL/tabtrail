# Tabtrail integration for Zsh (compsys). Protocol: tabtrail1.
#
# This file is static: it never evaluates candidate text or helper output;
# candidates are handed to `compadd`, which applies Zsh's own quoting.
# `__tabtrail_bin` is defined by the generated header above this text.
#
# Precedence: commands with their own compsys completion keep it. Tabtrail
# handles the `-default-` context (commands without completion) and calls
# the previous `-default-` handler (normally `_default`, i.e. files) when
# files are appropriate or Tabtrail has nothing to offer.

if [[ -o interactive ]]; then

__tabtrail_files() {
  local prev=${__tabtrail_prev_default:-_default}
  if [[ $prev != __tabtrail_complete ]] && (( $+functions[$prev] )); then
    "$prev" "$@"
  else
    _files "$@"
  fi
}

__tabtrail_complete() {
  local cmd=${words[1]} kind=
  if (( $+aliases[$cmd] )); then kind=alias
  elif (( $+functions[$cmd] )); then kind=function
  elif (( $+builtins[$cmd] )); then kind=builtin
  elif (( ${reswords[(Ie)$cmd]} )); then kind=keyword
  fi
  # Inside an open quote compsys hands widgets a dequoted LBUFFER whose
  # current word equals PREFIX; put the opening quote back so the helper
  # sees what the user typed (e.g. '$(x is text, not a substitution).
  local before=$LBUFFER q=${compstate[quote]-}
  if [[ -n $q ]]; then
    local plen=${#PREFIX}
    if (( plen == 0 )); then
      before+=$q
    elif [[ ${before[-plen,-1]} == "$PREFIX" ]]; then
      before="${before[1,-plen-1]}${q}${PREFIX}"
    else
      __tabtrail_files "$@"
      return
    fi
  fi
  local raw
  raw=$(command "$__tabtrail_bin" complete --shell zsh --before "$before" --command-kind "$kind" 2>/dev/null)
  local -a reply
  reply=("${(@0)raw}")
  if [[ ${reply[1]-} != tabtrail1 ]]; then
    __tabtrail_files "$@"
    return
  fi
  local flags=${reply[2]} rawstrip=${reply[3]} logstrip=${reply[4]} n=${reply[5]}
  [[ $n == <-> && $rawstrip == <-> && $logstrip == <-> ]] || { __tabtrail_files "$@"; return }
  # Inside quotes compsys hands us a dequoted PREFIX.
  local strip=$rawstrip
  [[ -n $q ]] && strip=$logstrip
  (( strip > 0 )) && compset -p $strip
  local -a sp_ins sp_disp ns_ins ns_disp
  local i base
  for (( i = 0; i < n; i++ )); do
    base=$(( 6 + 3 * i ))
    if [[ ${reply[base+2]} == 1 ]]; then
      ns_ins+=("${reply[base]}"); ns_disp+=("${reply[base+1]}")
    else
      sp_ins+=("${reply[base]}"); sp_disp+=("${reply[base+1]}")
    fi
  done
  local ret=1
  if (( $#sp_ins )); then compadd -J tabtrail -l -d sp_disp -a sp_ins && ret=0; fi
  if (( $#ns_ins )); then compadd -J tabtrail -S '' -l -d ns_disp -a ns_ins && ret=0; fi
  if [[ $flags == *f* ]] && { (( ret != 0 )) || [[ $PREFIX != -* ]] }; then
    __tabtrail_files "$@" && ret=0
  fi
  return ret
}

__tabtrail_register() {
  (( $+functions[compdef] )) || return 1
  if [[ ${_comps[-default-]-} != __tabtrail_complete ]]; then
    typeset -g __tabtrail_prev_default=${_comps[-default-]:-_default}
    compdef __tabtrail_complete -default-
  fi
  return 0
}

__tabtrail_register
# Also (re)register at the first prompt: compinit may run after this file
# (registration deferred) or run again later and reset `_comps`.
autoload -Uz add-zsh-hook
__tabtrail_first_prompt() {
  __tabtrail_register && add-zsh-hook -d precmd __tabtrail_first_prompt
}
add-zsh-hook precmd __tabtrail_first_prompt

fi
