## NAME

audioctl — سرد أجهزة الصوت وتغيير ضوابطها

## SYNOPSIS

`audioctl [devices]`

`audioctl streams [-a]`

`audioctl default <device>`

`audioctl level <device> <level>`

`audioctl mute <device>`

`audioctl unmute <device>`

## DESCRIPTION

يسرد المخارج والمداخل التي تراها هذه الجلسة: معرّف كلٍّ منها لهذا الإقلاع،
وهل هو الافتراضي لاتجاهه، ومستواه وكتمه، والمعدّل الذي يعمل به، والإطارات التي
فقدها، وموقعه واسمه. يسرد `streams` تدفقاتك أنت، ومع `--all` تدفقات جميع
الأطراف.

تغيّر `default` و`level` و`mute` و`unmute` ضوابط جهاز واحد. يُسمّى الجهاز بمرجع
`audio:`: ‏`audio:sink/default` أو `audio:source/default` للافتراضي الآن،
و`audio:sink/<id>` لهذا الإقلاع، أو `audio:sink/<location>` أينما كان الجهاز، كما
تسمّيها القائمة. المستوى بالديسيبل حتى الجزء من مئة، صفر أو أقل، مثل `-6` أو
`-3.5`؛ ولا يحتاج المستوى السالب إلى `--`.

ضوابط الجهاز ملك الغرفة التي يخدمها. للجلسة التي تشغل تلك الغرفة أن تغيّرها،
ولأيٍّ كان ذلك ما دامت الغرفة غير مشغولة، ولا لأحد ما دامت محجوبة؛ والرفض يقول
ذلك. ما تضبطه الجلسة ملكها: حين تشغل جلسة أخرى الغرفة يتنحّى، ويعود حين تعود.
المستوى الذي يبدأ به كل جهاز والأجهزة المفضّلة افتراضيًا هي إعدادات الجهاز
`audio.level` و`audio.output` و`audio.input`، ويضبطها `configure`.

على المعلومات القياسية (fd 3) يكتب `audioctl streams` سجلّ `omission` حين لا
يسرد إلا تدفقاتك أنت.

## OPTIONS

- `-a, --all` — مع `streams`، تدفقات جميع الأطراف؛ ويلزم لذلك `CAP_SYSINFO_GLOBAL`.
- `-h, -?, --help` — عرض المساعدة المختصرة لهذا الأمر.
- `--version` — عرض الإصدار، ثم الخروج.

## EXAMPLES

- `audioctl` — سرد المخارج والمداخل.
- `audioctl level audio:sink/default -10` — ضبط المخرج الافتراضي على عشرة ديسيبل دون الأقصى.
- `audioctl mute audio:source/default` — كتم المدخل الافتراضي.
- `audioctl default audio:sink/2` — جعل المخرج 2 هو الافتراضي.
- `audioctl streams --all` — سرد تدفقات جميع الأطراف.

## EXIT STATUS

- `0` — اكتمل الأمر.
- `1` — رُفض، أو تعذّر تنفيذه.
- `2` — لم يُفهم سطر الأوامر.

## ENVIRONMENT

- `LANG` — اللغة المفضّلة للمساعدة المختصرة (وسم BCP-47 مثل `fr-FR`).

## SEE ALSO

- `play`
- `configure`
- `sysinfo`
- `man`
