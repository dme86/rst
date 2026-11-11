# rst OSC 133 shell integration (source at end of config.fish).
status is-interactive; or return
test "$TERM_PROGRAM" = rst; or return
set -q __rst_shell_integration; and return
set -g __rst_shell_integration 1

# Preserve themed prompts and append B after their visible text.
if functions -q fish_prompt
    functions --copy fish_prompt __rst_original_fish_prompt
    function fish_prompt
        printf '\e]133;A\a'
        # OSC 7 updates the tab's working directory, without polling.
        printf '\e]7;file://localhost%s\a' "$PWD"
        __rst_original_fish_prompt
        printf '\e]133;B\a'
    end
end

function __rst_preexec --on-event fish_preexec
    # Fish supplies the command line; send just a validated first token.
    set -l line (string trim -- "$argv[1]")
    set -l first (string match -r '^[A-Za-z0-9_./+-]+' -- "$line")
    if test -n "$first"
        set -l program (string replace -r '^.*/' '' -- "$first")
        if test -n "$program"
            printf '\e]133;E;%s\a' "$program"
        end
    end
    printf '\e]133;C\a'
end

function __rst_postexec --on-event fish_postexec
    printf '\e]133;D\a'
end
