# Tabtrail integration for Fish (>= 3.2). Protocol: tabtrail1.
#
# This file is static: it never evaluates candidate text or helper output.
# The helper prints `candidate<TAB>description` lines (the `complete -a`
# contract) with tabs, newlines and control characters removed; Fish applies
# its own escaping on insertion. `__tabtrail_bin` is defined by the
# generated header above this text.
#
# Precedence: a wildcard path completion is consulted for every command,
# but Tabtrail returns nothing for commands that have their own Fish
# completions. Fish's own file completion stays available.

status is-interactive; or return

set -g __tabtrail_native_cmds

# Queries the helper for `line`; prints its raw output (header line first).
function __tabtrail_query -a line
    set -l toks
    if test (string split -f1 . -- $version) -ge 4
        set toks (commandline -xpc)
    else
        set toks (commandline -opc)
    end
    set -l cmd $toks[1]
    test -n "$cmd"; or return
    contains -- $cmd $__tabtrail_native_cmds; and return
    # Native completions take precedence.
    if test (count (complete -c $cmd)) -gt 0
        set -g -a __tabtrail_native_cmds $cmd
        return
    end
    set -l kind (type -t -- $cmd 2>/dev/null)
    command $__tabtrail_bin complete --shell fish --before "$line" --command-kind "$kind" 2>/dev/null
end

# Each entry queries the helper itself (about 0.2 ms). Sharing one result
# between them is unsafe: Fish does not evaluate the no-files condition on
# every Tab, so a cached answer could leak into a later request.
function __tabtrail_complete
    set -l line (commandline -cp | string collect)
    set -l out (__tabtrail_query "$line")
    string match -q 'tabtrail1*' -- "$out[1]"; or return
    set -l n (count $out)
    if test $n -gt 1
        printf '%s\n' $out[2..$n]
    end
end

# True when Tabtrail knows file names are wrong at this position.
function __tabtrail_nofiles
    set -l line (commandline -cp | string collect)
    set -l out (__tabtrail_query "$line")
    string match -q 'tabtrail1*' -- "$out[1]"; or return 1
    set -l flags (string split -f2 \t -- "$out[1]")
    not string match -q '*f*' -- "$flags"
end

if not set -q __tabtrail_registered
    set -g __tabtrail_registered 1
    complete -p '*' -a '(__tabtrail_complete)'
    complete -p '*' -f -n __tabtrail_nofiles
end
