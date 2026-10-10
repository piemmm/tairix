## NAME

audioctl — להציג את התקני השמע ולשנות את הבקרות שלהם

## SYNOPSIS

`audioctl [devices]`

`audioctl streams [-a]`

`audioctl default <device>`

`audioctl level <device> <level>`

`audioctl mute <device>`

`audioctl unmute <device>`

## DESCRIPTION

מציג את הפלטים והקלטים שהפעלה זו רואה: המזהה של כל אחד באתחול זה, האם הוא
ברירת המחדל של כיוונו, העוצמה וההשתקה שלו, הקצב שבו הוא פועל, המסגרות שאיבד,
המיקום והשם שלו. `streams` מציג את הזרמים שלך, ועם `--all` את הזרמים של כל
הישויות.

`default`, `level`, `mute` ו-`unmute` משנים את הבקרות של התקן אחד. התקן נקרא
בהפניית `audio:`: ‏`audio:sink/default` או `audio:source/default` לברירת המחדל
כעת, `audio:sink/<id>` לאתחול זה, או `audio:sink/<location>` היכן שההתקן נמצא,
כפי שהרשימה מכנה אותם. עוצמה נמדדת בדציבלים עד המאית, 0 או פחות, כמו `-6` או
`-3.5`; עוצמה שלילית אינה צריכה `--`.

הבקרות של התקן שייכות לחדר שהוא משרת. ההפעלה שמחזיקה בחדר רשאית לשנות אותן,
כל אחד רשאי כל עוד החדר פנוי, ואיש אינו רשאי כל עוד הוא מעוכב; סירוב אומר
זאת. מה שהפעלה קובעת שייך לה: כל עוד הפעלה אחרת מחזיקה בחדר, הוא נסוג, וחוזר
כשהיא חוזרת. העוצמה שבה כל התקן מתחיל וההתקנים המועדפים כברירת מחדל הם הגדרות
המכונה `audio.level`, `audio.output` ו-`audio.input`, ש-`configure` קובע.

במידע התקני (fd 3) `audioctl streams` כותב רשומת `omission` כשהוא מציג רק את
הזרמים שלך.

## OPTIONS

- `-a, --all` — עם `streams`, הזרמים של כל הישויות; נדרש `CAP_SYSINFO_GLOBAL`.
- `-h, -?, --help` — להציג את העזרה הקצרה של פקודה זו.
- `--version` — להציג את הגרסה ולצאת.

## EXAMPLES

- `audioctl` — להציג את הפלטים והקלטים.
- `audioctl level audio:sink/default -10` — לקבוע את הפלט ברירת המחדל עשרה דציבלים מתחת למלא.
- `audioctl mute audio:source/default` — להשתיק את הקלט ברירת המחדל.
- `audioctl default audio:sink/2` — להפוך את פלט 2 לברירת המחדל.
- `audioctl streams --all` — להציג את הזרמים של כל הישויות.

## EXIT STATUS

- `0` — הפקודה הושלמה.
- `1` — היא נדחתה, או שלא ניתן היה לבצע אותה.
- `2` — שורת הפקודה לא הובנה.

## ENVIRONMENT

- `LANG` — השפה המועדפת לעזרה הקצרה (תג BCP-47 כגון `fr-FR`).

## SEE ALSO

- `play`
- `configure`
- `sysinfo`
- `man`
