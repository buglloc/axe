FROM ghcr.io/gohugoio/hugo:v0.166.0 AS build

WORKDIR /project

COPY --chown=hugo:hugo . .

RUN hugo --gc --minify --cleanDestinationDir

FROM docker.io/library/nginx:1.31.5-alpine3.24

COPY default.conf /etc/nginx/conf.d/default.conf

COPY --from=build /project/public /usr/share/nginx/html

EXPOSE 80

CMD ["nginx", "-g", "daemon off;"]
