# Tabtrail integration for Bash (>= 4.4). Protocol: tabtrail1.
#
# This file is static: it never evaluates candidate text or helper output.
# `__tabtrail_bin` is defined by the generated header above this text.
#
# Precedence: commands with their own completion keep it. Tabtrail registers
# itself as the *default* completion (`complete -D`). When bash-completion's
# lazy loader is installed, it is called first; Tabtrail only takes over
# commands for which the loader found nothing but the generic "minimal"
# completion. If Tabtrail has nothing to offer, Bash's own filename
# completion runs (`-o bashdefault -o default`).

# shellcheck disable=SC2154 # __tabtrail_bin is set by the generated header
if [[ -n ${BASH_VERSION-} && $- == *i* ]] &&
    ((BASH_VERSINFO[0] > 4 || (BASH_VERSINFO[0] == 4 && BASH_VERSINFO[1] >= 4))); then

    __tabtrail_complete() {
        local before=${COMP_LINE:0:COMP_POINT}
        local cmd=${COMP_WORDS[0]-} kind=
        # Classify the command word without forking (no `type -t` subshell).
        if [[ -n $cmd ]]; then
            if [[ -n ${BASH_ALIASES[$cmd]+set} ]]; then
                kind='alias'
            elif declare -F -- "$cmd" >/dev/null 2>&1; then
                kind='function'
            fi
        fi
        local -a __tt_reply=()
        mapfile -d '' -t __tt_reply < <(
            command "$__tabtrail_bin" complete --shell bash --before "$before" \
                --wordbreaks "$COMP_WORDBREAKS" --command-kind "$kind" 2>/dev/null
        )
        if [[ ${__tt_reply[0]-} != tabtrail1 ]]; then
            return 1 # helper missing or failed: Bash falls back to files
        fi
        local flags=${__tt_reply[1]-} n=${__tt_reply[2]-0}
        [[ $n =~ ^[0-9]+$ ]] || n=0
        COMPREPLY=("${__tt_reply[@]:3:n}")
        if ((${#COMPREPLY[@]} == 0)); then
            if [[ $flags != *f* ]]; then
                # Nothing valid here, and file names would be wrong.
                compopt +o default +o bashdefault 2>/dev/null
            fi
            return 0
        fi
        if [[ $flags == *s* ]]; then
            compopt -o nospace 2>/dev/null
        fi
        return 0
    }

    __tabtrail_default() {
        local cmd=${COMP_WORDS[0]-} ret spec
        if [[ -n ${__tabtrail_prev_default-} ]] && declare -F -- "$__tabtrail_prev_default" >/dev/null 2>&1; then
            "$__tabtrail_prev_default" "$@"
            ret=$?
            if ((ret == 124)); then
                spec=$(complete -p -- "$cmd" 2>/dev/null)
                case " $spec " in
                *" -F _minimal "* | *" -F _comp_complete_minimal "* | "  ")
                    # The loader found no native completion for this command.
                    complete -F __tabtrail_complete -o bashdefault -o default -- "$cmd" 2>/dev/null
                    ;;
                esac
                return 124 # let Bash retry with whatever is registered now
            fi
            if ((${#COMPREPLY[@]})); then
                return "$ret"
            fi
        fi
        __tabtrail_complete "$@"
    }

    __tabtrail_setup() {
        local spec prev=
        spec=$(complete -p -D 2>/dev/null)
        if [[ -z $spec ]]; then
            : # no default completion yet: Tabtrail becomes the default
        elif [[ $spec =~ ^complete\ -F\ ([^ ]+)\ -D$ ]]; then
            # A plain function default (bash-completion's lazy loader):
            # Tabtrail wraps it and calls it first.
            prev=${BASH_REMATCH[1]}
            case $prev in
            __tabtrail_*) return 0 ;; # already installed (re-sourced rc file)
            esac
        else
            # Any other default (-W words, -C command, -o options, ...)
            # cannot be delegated faithfully: leave it untouched.
            case $spec in
            *" -F __tabtrail_"*) ;;
            *) __tabtrail_inactive="existing default completion kept: $spec" ;;
            esac
            return 0
        fi
        __tabtrail_prev_default=$prev
        complete -D -F __tabtrail_default -o bashdefault -o default
    }

    __tabtrail_setup
fi
