# UT1 Emploi du temps

Ce script est un outil permettant de récupérer l'emploi du temps de l'université UT1 et de l'exporter au format iCalendar.

## Notes

Ce script n'effectue pas de d'appels API, ni de requêtes directes aux serveurs d'UT1, dans la mesure où c'est un serveur Tomcat utilisant les jetons GWT.  

Il utilise donc un navigateur "headless" (ici Chrome), et simule une navigation sur le site d'UT1 pour récupérer les données.  
  
La page web distribue les événements sans fournir leurs horaires, seulement les positions par rapport à l'élément parent. Ils sont donc convertis en heure de début et durée.  
  
Le script retourne un fichier iCalendar (.ics), distribuable sur un serveur web.