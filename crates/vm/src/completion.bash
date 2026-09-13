_clap_complete_%NAME%() {
    local IFS=$'\013'
    if compopt +o nospace 2> /dev/null; then
        local space=false
    else
        local space=true
    fi
    # Rejoin the words COMP_WORDBREAKS split, such as debian:trixie and --firmware=uefi.
    local line=${COMP_LINE:0:COMP_POINT} words=() prefix i gap raw
    for ((i = 0; i < COMP_CWORD; i++)); do
        [[ $line =~ ^[[:space:]]* ]]
        gap=${BASH_REMATCH[0]}
        line=${line:${#gap}}
        raw=${COMP_WORDS[i]}
        if [[ -n $gap || ${#words[@]} -eq 0 ]]; then
            words+=("$raw")
        else
            words[-1]+=$raw
        fi
        line=${line:${#raw}}
    done
    raw=${COMP_WORDS[COMP_CWORD]}
    [[ $line =~ ^[[:space:]]* ]]
    if [[ -n ${BASH_REMATCH[0]} || ${#words[@]} -eq 0 ]]; then
        prefix=
    else
        prefix=${words[-1]}
        unset 'words[-1]'
    fi
    # A cursor just after a break character leaves bash's current word empty.
    [[ -n $raw && -z ${raw//[$COMP_WORDBREAKS]/} ]] && prefix+=$raw
    words+=("$prefix$2")
    COMPREPLY=( $( \
        _CLAP_IFS="$IFS" \
        _CLAP_COMPLETE_INDEX="$((${#words[@]} - 1))" \
        _CLAP_COMPLETE_COMP_TYPE="$COMP_TYPE" \
        _CLAP_COMPLETE_SPACE="$space" \
        %VAR%="bash" \
        "%COMPLETER%" -- "${words[@]}" \
    ) )
    if [[ $? != 0 ]]; then
        unset COMPREPLY
        return
    fi
    if [[ $space == false ]] && [[ "${COMPREPLY-}" =~ [=/:]$ ]]; then
        compopt -o nospace
    fi
    # Readline replaces only the text after the break, so candidates lose the prefix.
    [[ -n $prefix ]] && COMPREPLY=("${COMPREPLY[@]#"$prefix"}")
}
complete -o nospace -o bashdefault -o nosort -F _clap_complete_%NAME% %BIN%
