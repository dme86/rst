# rst OSC 133 shell integration (source from an interactive bash rc).
# Shell annotations stay invisible and never enter command output.
[[ $- == *i* && ${TERM_PROGRAM:-} == rst ]] || return 0
[[ ${__rst_shell_integration:-} == 1 ]] && return 0
__rst_shell_integration=1

__rst_precmd() {
    printf '\033]133;D\a\033]133;A\a'
    printf '\033]7;file://localhost%s\a' "$PWD"
    # macOS ships Bash 3.2 without PS0. A temporary DEBUG trap marks the
    # next foreground command, then removes itself before execution.
    if (( BASH_VERSINFO[0] < 4 || (BASH_VERSINFO[0] == 4 && BASH_VERSINFO[1] < 4) )); then
        if [[ ${__rst_debug_trap_available:-0} == 1 ]]; then
            __rst_ready=1
        fi
    fi
}
__rst_preexec() {
    [[ ${__rst_ready:-0} == 1 ]] || return 0
    __rst_ready=0
    local executable="${1%% *}"
    executable="${executable##*/}"
    if [[ "$executable" =~ ^[a-zA-Z0-9._+-]+$ ]]; then
        printf '\033]133;E;%s\a' "$executable"
    fi
    printf '\033]133;C\a'
}
# Append our hook *last* so an older Bash DEBUG trap does not fire while
# running an existing prompt function. Never overwrite a user's DEBUG trap.
if (( BASH_VERSINFO[0] > 4 || (BASH_VERSINFO[0] == 4 && BASH_VERSINFO[1] >= 4) )); then
    # BASH_COMMAND is expanded by Bash when PS0 is displayed.
    # A malformed command name is rejected by rst's OSC parser.
    PS0="${PS0:-}"'\033]133;E;${BASH_COMMAND%% *}\a\033]133;C\a'
else
    __rst_debug_trap_available=0
    if [[ -z $(trap -p DEBUG) ]]; then
        __rst_debug_trap_available=1
        __rst_ready=0
        trap '__rst_preexec "$BASH_COMMAND"' DEBUG
    fi
fi
PROMPT_COMMAND="${PROMPT_COMMAND:+$PROMPT_COMMAND; }__rst_precmd"
PS1="${PS1}"'\[\033]133;B\a\]'
