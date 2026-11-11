# rst OSC 133 shell integration (source at end of .zshrc).
[[ -o interactive && ${TERM_PROGRAM:-} == rst ]] || return 0
[[ ${__rst_shell_integration:-} == 1 ]] && return 0
typeset -g __rst_shell_integration=1
autoload -Uz add-zsh-hook

__rst_preexec() {
    local executable="${1%% *}"
    executable="${executable:t}"
    if [[ -n "$executable" && "$executable" != *[!A-Za-z0-9._+-]* ]]; then
        printf '\e]133;E;%s\a' "$executable"
    fi
    print -n -- 
__rst_precmd() {
    # D also closes a command after an unusual shell interruption. rst
    # ignores unmatched D markers; A always identifies the next prompt.
    print -n -- 
}
add-zsh-hook preexec __rst_preexec
add-zsh-hook precmd __rst_precmd
# %{%} declares invisible prompt bytes, preserving zsh cursor placement.
PROMPT="${PROMPT}"$'%{\e]133;B\a%}'
\e]133;C\a'
}
__rst_precmd() {
    # D also closes a command after an unusual shell interruption. rst
    # ignores unmatched D markers; A always identifies the next prompt.
    print -n -- $'\e]133;D\a\e]133;A\a'
}
add-zsh-hook preexec __rst_preexec
add-zsh-hook precmd __rst_precmd
# %{%} declares invisible prompt bytes, preserving zsh cursor placement.
PROMPT="${PROMPT}"$'%{\e]133;B\a%}'
\e]133;D\a\e]133;A\a'
    printf '\e]7;file://localhost%s\a' "$PWD"
}
add-zsh-hook preexec __rst_preexec
add-zsh-hook precmd __rst_precmd
# %{%} declares invisible prompt bytes, preserving zsh cursor placement.
PROMPT="${PROMPT}"$'%{\e]133;B\a%}'
\e]133;C\a'
}
__rst_precmd() {
    # D also closes a command after an unusual shell interruption. rst
    # ignores unmatched D markers; A always identifies the next prompt.
    print -n -- $'\e]133;D\a\e]133;A\a'
}
add-zsh-hook preexec __rst_preexec
add-zsh-hook precmd __rst_precmd
# %{%} declares invisible prompt bytes, preserving zsh cursor placement.
PROMPT="${PROMPT}"$'%{\e]133;B\a%}'
