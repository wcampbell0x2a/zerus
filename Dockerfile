FROM alpine:3.22

ARG BIN=zerus

# Serve the mirror as a user without privileges.
RUN adduser -D -H -u 10001 zerus \
    && mkdir /mirror \
    && chown zerus:zerus /mirror

COPY --chmod=755 ${BIN} /usr/local/bin/zerus

USER zerus
WORKDIR /mirror

VOLUME ["/mirror"]
EXPOSE 8080

ENTRYPOINT ["/usr/local/bin/zerus"]
CMD ["serve", "/mirror", "--bind", "0.0.0.0:8080"]
