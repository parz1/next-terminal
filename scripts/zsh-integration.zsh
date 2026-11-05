# 由 Next Terminal 在首次提示符加载，也可手动 source；不写用户配置或启用命令框。
[[ -o interactive ]] || return
autoload -Uz add-zsh-hook

function _next_terminal_control_close() {
    if [[ -n ${_next_terminal_control_fd-} ]]; then
        zle -F "$_next_terminal_control_fd" 2>/dev/null
        exec {_next_terminal_control_fd}>&-
        unset _next_terminal_control_fd
    fi
}

function _next_terminal_control_send() {
    if [[ -n ${_next_terminal_control_fd-} ]]; then
        { builtin printf '%s\n' "$1" >&$_next_terminal_control_fd } 2>/dev/null || _next_terminal_control_close
    fi
    return 0
}

# 仅从 socket 读 CHECK，不从终端读取探测字符，也不改变 BUFFER。
function _next_terminal_check_ready() {
    local line kind request generation extra ready=0
    if ! IFS=' ' read -r -t 0.1 -u "$1" kind request generation extra; then
        _next_terminal_control_close
        return 0
    fi
    if [[ $kind != CHECK || $request != <-> || $generation != <-> || -n $extra ]]; then
        _next_terminal_control_close
        return 0
    fi
    if [[ ${_next_terminal_prompt_cycle:-0} == 1 && $CONTEXT == start && -z $BUFFER && -z $PREBUFFER && $PENDING == 0 && ${KEYS_QUEUED_COUNT:-0} == 0 && ${_next_terminal_control_pasting:-0} == 0 ]]; then
        ready=1
    fi
    _next_terminal_control_send "READY $request $generation $ready"
    return 0
}

function _next_terminal_preexec() {
    _next_terminal_control_send 'STATE running'
    _next_terminal_prompt_cycle=0
    _next_terminal_command_running=1
    builtin printf '\e]133;C\a'
}

# OSC 7 报告当前目录，供窗口标题和路径标签显示；只用于展示，不参与输入授权。
# 按字节做百分号编码，空格、中文、分号等都能无歧义地还原。
function _next_terminal_report_cwd() {
    emulate -L zsh
    local LC_ALL=C
    local i byte hex encoded=''
    for (( i = 1; i <= ${#PWD}; i++ )); do
        byte=${PWD[i]}
        if [[ $byte == [A-Za-z0-9/._~-] ]]; then
            encoded+=$byte
        else
            builtin printf -v hex '%%%02X' "'$byte"
            encoded+=$hex
        fi
    done
    builtin printf '\e]7;file://%s%s\a' "${HOST}" "$encoded"
}

function _next_terminal_precmd() {
    local command_status=$?
    if [[ ${_next_terminal_command_running:-0} == 1 ]]; then
        builtin printf '\e]133;D;%d\a' "$command_status"
    fi
    _next_terminal_command_running=0
    _next_terminal_prompt_cycle=1
    _next_terminal_report_cwd
    builtin printf '\e]133;A\a'
    _next_terminal_control_send 'STATE prompt'
}

# CONTEXT 由 ZLE 提供；只有 precmd 后、preexec 前的 start/cont 才算 shell 命令行。
# 只发送上下文和空/非空，不发送 BUFFER 或 PREBUFFER 的内容。
function _next_terminal_observe_line() {
    _next_terminal_control_pasting=0
    local edit_state=other
    if [[ ${_next_terminal_prompt_cycle:-0} == 1 ]]; then
        case $CONTEXT in
            start|cont)
                local empty=0
                [[ -z $BUFFER ]] && empty=1
                edit_state="$CONTEXT;$empty"
                ;;
        esac
    fi
    builtin printf '\e]777;next-terminal;zle;1;%s\a' "$edit_state"
    case $edit_state in
        'start;1') _next_terminal_control_send 'STATE primary-empty';;
        'start;0') _next_terminal_control_send 'STATE primary-text';;
        'cont;1') _next_terminal_control_send 'STATE continuation-empty';;
        'cont;0') _next_terminal_control_send 'STATE continuation-text';;
        *) _next_terminal_control_send 'STATE other';;
    esac
    return 0
}

function _next_terminal_finish_line() {
    _next_terminal_control_send 'STATE inactive'
    builtin printf '\e]777;next-terminal;zle;1;inactive\a'
}

# 重复 source 不叠加同名 hook，也不替换用户已有的其他 hook。
add-zsh-hook -d preexec _next_terminal_preexec
add-zsh-hook -d precmd _next_terminal_precmd
add-zsh-hook preexec _next_terminal_preexec
add-zsh-hook precmd _next_terminal_precmd

# 保留其他已注册的 ZLE hook，不替换用户的按键绑定。
autoload -Uz add-zle-hook-widget
zle -N _next_terminal_observe_line
zle -N _next_terminal_finish_line
add-zle-hook-widget line-init _next_terminal_observe_line
add-zle-hook-widget line-pre-redraw _next_terminal_observe_line
add-zle-hook-widget line-finish _next_terminal_finish_line

# zsh 自带的 bracketed-paste-magic 会在 zle -R 后 read-command，直到下一按键
# 才触发 pre-redraw。此时 PASTED 已处理完成，但还未插入 BUFFER；只补报空/非空。
function _next_terminal_observe_paste() {
    if [[ ${_next_terminal_prompt_cycle:-0} == 1 && ($CONTEXT == start || $CONTEXT == cont) ]]; then
        local empty=1
        [[ -n $BUFFER || -n $PASTED ]] && empty=0
        builtin printf '\e]777;next-terminal;zle;1;%s;%s\a' "$CONTEXT" "$empty"
        if [[ $CONTEXT == start ]]; then
            if [[ $empty == 1 ]]; then _next_terminal_control_send 'STATE primary-empty'; else _next_terminal_control_send 'STATE primary-text'; fi
        fi
    fi
    return 0
}
typeset -a _next_terminal_paste_hooks
zstyle -a :bracketed-paste-magic paste-finish _next_terminal_paste_hooks
if (( ! ${_next_terminal_paste_hooks[(Ie)_next_terminal_observe_paste]} )); then
    zstyle :bracketed-paste-magic paste-finish "${_next_terminal_paste_hooks[@]}" _next_terminal_observe_paste
fi
unset _next_terminal_paste_hooks

function _next_terminal_paste_started() {
    _next_terminal_control_pasting=1
    _next_terminal_control_send 'STATE inactive'
    return 0
}
typeset -a _next_terminal_paste_init_hooks
zstyle -a :bracketed-paste-magic paste-init _next_terminal_paste_init_hooks
if (( ! ${_next_terminal_paste_init_hooks[(Ie)_next_terminal_paste_started]} )); then
    zstyle :bracketed-paste-magic paste-init "${_next_terminal_paste_init_hooks[@]}" _next_terminal_paste_started
fi
unset _next_terminal_paste_init_hooks

# 路径和一次性握手令牌只由会话私有启动脚本设置，不导出给子 shell。
if [[ -z ${_next_terminal_control_fd-} && -n ${_next_terminal_control_path-} && -n ${_next_terminal_control_token-} ]]; then
    if zmodload zsh/net/socket && zsocket "$_next_terminal_control_path"; then
        typeset -g _next_terminal_control_fd=$REPLY
        _next_terminal_control_send "HELLO $_next_terminal_control_token"
        zle -N _next_terminal_check_ready
        zle -F -w "$_next_terminal_control_fd" _next_terminal_check_ready
    fi
    unset _next_terminal_control_path _next_terminal_control_token
fi
