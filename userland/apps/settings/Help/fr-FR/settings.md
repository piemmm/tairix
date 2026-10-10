## NAME

settings — configurer le bureau et cette machine

## SYNOPSIS

`settings`

## DESCRIPTION

Ouvre une fenêtre de bureau qui répertorie chaque catégorie de réglages de ce
système : ce qu'est la machine, l'apparence du bureau, l'écran, le réseau, les
périphériques de commande, les comptes qui l'utilisent et les volumes qu'elle
contient. Choisir une catégorie dans la barre latérale affiche son panneau. Une
catégorie qui contient plusieurs panneaux ouvre et ferme plutôt leur liste sur
place, et autant de ces listes que voulu peuvent rester ouvertes à la fois.

Settings ne détient aucune autorité propre. Chaque modification est soit une
demande à la session de bureau, qui possède les réglages de l'utilisateur, soit
une exécution ré-authentifiée de la commande qui écrit déjà ce magasin : rien
ici ne peut élever un privilège.

Une catégorie que ce système ne peut pas servir le dit clairement et nomme ce
qui devrait exister pour qu'elle le puisse. Un contrôle qui ne changerait rien
n'est jamais montré.

Saisissez du texte dans le champ de recherche au-dessus de la barre latérale
pour la filtrer sur les catégories et les réglages qu'un mot atteint. `Tab` et
`Shift+Tab` déplacent le focus entre le champ de recherche, le fil de
navigation, la barre latérale et le panneau ; `Up` et `Down` parcourent la
barre latérale et `Enter` ouvre la ligne. `Right` et `Left` ouvrent et ferment
la liste de la catégorie sous le curseur. Une fenêtre trop étroite abandonne la
barre latérale, et la première miette du fil de navigation liste alors les
catégories.

Il se lance depuis la ligne *Settings…* du menu système du bureau, depuis la
Bibliothèque de programmes, ou par son nom depuis un shell. Il exige une
session graphique en cours : sans elle, le canal de fenêtre est inaccessible et
il signale le refus sur le flux d'erreur standard puis se termine.

Son affiche chaque sortie et chaque entrée avec son choix par défaut, son
niveau et sa sourdine, ainsi que ce qui enregistre : vos propres
enregistrements par leur nom, et combien en font les autres. Ses réglages
changent le périphérique sur-le-champ, et seulement tant que votre session
tient la salle qu'il dessert ; un périphérique de la salle d'une autre session
est affiché mais ne peut pas être modifié ici. Votre bureau retient ce que
vous réglez.

## EXIT STATUS

Zéro après une fermeture propre ; non nul lorsque le canal de fenêtre ou la
région de trames partagée a été refusé (la raison est indiquée sur le flux
d'erreur standard).
