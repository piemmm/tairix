## NAME

tail — afficher la fin des fichiers

## SYNOPSIS

`tail [option...] [file...]`

## DESCRIPTION

Affiche les 10 dernières lignes de chaque `file` sur la sortie
standard. Avec plusieurs `file`, chaque partie est précédée d'un
en-tête `==> file <==`. Sans `file`, ou quand `file` vaut `-`, l'entrée
standard est lue.

`-n` et `-c` changent la quantité affichée : un compte simple (ou écrit
avec un `-` en tête) affiche les `num` dernières lignes ou derniers
octets ; un compte écrit avec un `+` en tête affiche tout **à partir**
de la ligne ou de l'octet `num` (à compter de 1) jusqu'à la fin. Un
compte peut porter un suffixe multiplicateur : `b` (512), `kB` (1000),
`K` (1024), `MB`, `M`, `GB`, `G`, et ainsi de suite pour `T`, `P`, `E`,
`Z`, `Y`, `R`, `Q` (une lettre seule multiplie par des puissances de
1024 ; avec `B` par des puissances de 1000 ; avec `iB` par des
puissances de 1024).

La forme historique en premier argument `tail -num` / `tail +num` (avec
une lettre finale `b`/`c`/`l` facultative) est acceptée, comme dans
l'outil GNU.

Le mode suivi garde chaque fichier ouvert et affiche les nouvelles
données à mesure que le fichier grandit. `-f` suit le fichier ouvert par
son descripteur — après un renommage ou un déplacement, c'est toujours
le même fichier qui est suivi. `-F` suit le *nom* : quand le fichier
subit une rotation (renommé ailleurs et remplacé par un nouveau fichier)
il rouvre le nouveau fichier, et `--retry` continue d'essayer un nom
qui n'existe pas encore. Un suivi se bloque jusqu'à ce que le fichier
change réellement — il ne fait jamais tourner le CPU à vide.
`--pid=PID` arrête le suivi dès que le processus `PID` se termine,
vérifié toutes les `--sleep-interval` (secondes, 1 par défaut).
`--max-unchanged-stats=N` règle combien de cycles sans changement
s'écoulent avant que `-F` revérifie le nom en quête d'une rotation (5
par défaut). Une troncature (le fichier rétrécit) est signalée et le
fichier est de nouveau suivi depuis son nouveau début.

Quand du contenu en tête n'est pas affiché, un enregistrement
consultatif est écrit sur le flux d'information standard (fd 3) ; il ne
change jamais la sortie ni le code de sortie. Un fichier illisible est
signalé sur la sortie d'erreur et l'exécution continue avec le fichier
suivant.

Sur un terminal, chaque nom affiché est mis entre guillemets là où un
shell en aurait besoin, et tout caractère de contrôle ou autre caractère
invisible qu'il contient est écrit sous forme de séquence d'échappement,
de sorte qu'un nom ne peut ni commander ni réorganiser l'écran ; GNU
l'affiche tel quel. Un tube ou un fichier reçoit le nom octet pour
octet.

## OPTIONS

- `-c, --bytes <num>` — afficher les `num` derniers octets de chaque
  fichier ; avec un `+` en tête, tout à partir de l'octet `num`.
- `-n, --lines <num>` — afficher les `num` dernières lignes de chaque
  fichier ; avec un `+` en tête, tout à partir de la ligne `num`.
- `-q, --quiet, --silent` — ne jamais afficher les en-têtes
  `==> file <==`.
- `-v, --verbose` — toujours afficher les en-têtes `==> file <==`.
- `-z, --zero-terminated` — les lignes sont délimitées par NUL au lieu
  du saut de ligne.
- `-f, --follow[=descriptor]` — garder le fichier ouvert et afficher les
  données à mesure qu'elles sont ajoutées, en le suivant par
  descripteur.
- `-F` — suivre par nom (`--follow=name --retry`) : rouvrir le fichier
  après une rotation.
- `--follow=name` — suivre le nom plutôt que le descripteur.
- `--retry` — continuer d'essayer d'ouvrir un fichier pas encore
  accessible.
- `--pid <PID>` — pendant un suivi, s'arrêter dès que le processus `PID`
  meurt.
- `--sleep-interval <N>` — secondes entre les revérifications de
  `--pid`/rotation (fractions permises ; 1 par défaut).
- `--max-unchanged-stats <N>` — cycles sans changement avant que `-F`
  revérifie le nom en quête d'une rotation (5 par défaut).
- `-h, -?` — afficher l'aide courte de cette commande.

## EXAMPLES

- `tail log.txt` — afficher les 10 dernières lignes de `log.txt`.
- `tail -n 3 a b` — afficher les 3 dernières lignes de `a` et de `b`,
  chacune sous son en-tête.
- `tail -c 1K image` — afficher les 1024 derniers octets de `image`.
- `tail -n +5 notes` — afficher `notes` à partir de sa 5e ligne.
- `tail -f log.txt` — afficher les 10 dernières lignes, puis les
  nouvelles lignes à mesure qu'elles sont écrites.
- `tail -F /System/Logs/system` — suivre le journal système à travers
  les rotations.
- `tail -f --pid 1234 out` — suivre `out` jusqu'à la fin du processus
  1234.

## EXIT STATUS

- `0` — chaque fichier a été affiché (ou l'aide courte a été écrite).
- `1` — un fichier n'a pas pu être lu, ou la sortie n'a pas pu être
  délivrée. Une ligne de commande non comprise sort aussi avec `1`.

## ENVIRONMENT

- `LANG` — la locale préférée pour l'aide courte (une étiquette BCP-47
  telle que `fr-FR`).

## SEE ALSO

- `head`
- `cat`
- `wc`
- `man`
