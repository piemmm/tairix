## NAME

audioctl — 사운드 장치를 나열하고 그 설정을 바꾼다

## SYNOPSIS

`audioctl [devices]`

`audioctl streams [-a]`

`audioctl default <device>`

`audioctl level <device> <level>`

`audioctl mute <device>`

`audioctl unmute <device>`

## DESCRIPTION

이 세션이 볼 수 있는 출력 장치와 입력 장치를 나열한다. 각 장치의 이번 부팅
ID, 해당 방향의 기본 장치인지 여부, 레벨과 음소거, 동작 중인 속도, 잃어버린
프레임 수, 위치와 이름을 보여 준다. `streams`는 자신의 스트림을 나열하고,
`--all`을 붙이면 모든 주체의 스트림을 나열한다.

`default`, `level`, `mute`, `unmute`는 한 장치의 설정을 바꾼다. 장치는
`audio:` 참조로 가리킨다. `audio:sink/default` 또는 `audio:source/default`는
지금의 기본 장치, `audio:sink/<id>`는 이번 부팅의 장치, `audio:sink/<location>`
은 장치가 어디에 있든 그 장치를 가리키며, 모두 목록이 알려 주는 이름이다.
레벨은 0 이하의 데시벨로 100분의 1까지 쓰며, `-6`이나 `-3.5`처럼 쓴다. 음수
레벨에는 `--`가 필요 없다.

장치의 설정은 그 장치가 맡은 방의 것이다. 그 방을 가진 세션은 바꿀 수 있고,
방이 비어 있을 때는 누구나 바꿀 수 있으며, 보류 중일 때는 아무도 바꿀 수
없다. 거부되면 그렇다고 알려 준다. 세션이 정한 값은 그 세션의 것이다. 다른
세션이 방을 가진 동안에는 물러나고, 돌아오면 다시 적용된다. 각 장치의 시작
레벨과 기본으로 선호하는 장치는 기계 설정 `audio.level`, `audio.output`,
`audio.input`이며, `configure`가 설정한다.

표준 정보(fd 3)에는 자신의 스트림만 나열할 때 `audioctl streams`가
`omission` 레코드를 쓴다.

## OPTIONS

- `-a, --all` — `streams`와 함께 쓰며, 모든 주체의 스트림을 나열한다. `CAP_SYSINFO_GLOBAL`이 필요하다.
- `-h, -?, --help` — 이 명령 자체의 짧은 도움말을 보여 준다.
- `--version` — 버전을 보여 주고 끝낸다.

## EXAMPLES

- `audioctl` — 출력 장치와 입력 장치를 나열한다.
- `audioctl level audio:sink/default -10` — 기본 출력 장치를 최대보다 10데시벨 낮춘다.
- `audioctl mute audio:source/default` — 기본 입력 장치를 음소거한다.
- `audioctl default audio:sink/2` — 출력 장치 2를 기본으로 만든다.
- `audioctl streams --all` — 모든 주체의 스트림을 나열한다.

## EXIT STATUS

- `0` — 명령이 완료되었다.
- `1` — 거부되었거나 수행할 수 없었다.
- `2` — 명령줄을 이해하지 못했다.

## ENVIRONMENT

- `LANG` — 짧은 도움말에 쓸 언어(`fr-FR` 같은 BCP-47 태그).

## SEE ALSO

- `play`
- `configure`
- `sysinfo`
- `man`
